//! Local inference proxy.
//!
//! Routes AI requests to a user-configured OpenAI-compatible endpoint
//! (`DirectProvider`) instead of `app.warp.dev`, then translates the response
//! back into the `ResponseEvent` stream format the rest of the app expects.

mod openai;
mod tool_defs;
mod translate;

use std::sync::Arc;

use uuid::Uuid;
use warp_multi_agent_api as api;

use crate::ai::agent::api::{Event, ResponseStream};
use crate::server::server_api::AIApiError;

/// Run a single AI turn against a `DirectProvider`, returning a response stream
/// that is compatible with the rest of the Warp agent pipeline.
#[cfg(not(target_family = "wasm"))]
pub fn run_direct_inference(
    provider_config: ai::providers::DirectProviderConfig,
    request: api::Request,
) -> ResponseStream {
    let stream = async_stream::stream! {
        match run_inference_inner(provider_config, request).await {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(e) => {
                yield Err(Arc::new(e));
            }
        }
    };
    Box::pin(stream)
}

#[cfg(not(target_family = "wasm"))]
async fn run_inference_inner(
    config: ai::providers::DirectProviderConfig,
    request: api::Request,
) -> Result<Vec<Event>, AIApiError> {
    use http_client::Client;

    // ── Resolve IDs ────────────────────────────────────────────────────────────

    let conversation_id = if request.metadata.as_ref().map_or(true, |m| m.conversation_id.is_empty()) {
        Uuid::new_v4().to_string()
    } else {
        request
            .metadata
            .as_ref()
            .unwrap()
            .conversation_id
            .clone()
    };

    let (root_task_id, needs_create_task) =
        match request.task_context.as_ref().and_then(|tc| tc.tasks.first()) {
            Some(t) => (t.id.clone(), false),
            None => (Uuid::new_v4().to_string(), true),
        };

    // ── Build tool results to echo back to client ──────────────────────────────

    let input_tool_results = collect_input_tool_results(&request, &root_task_id);

    // ── Build OpenAI request ───────────────────────────────────────────────────

    let messages = translate::build_openai_messages(&request);

    let supported_tools: Vec<i32> = request
        .settings
        .as_ref()
        .map(|s| s.supported_tools.clone())
        .unwrap_or_default();

    let tool_defs = tool_defs::tool_definitions_for(&supported_tools);

    let tools = if tool_defs.is_empty() { None } else { Some(tool_defs) };

    let oai_request = openai::ChatRequest {
        model: config.model_id.clone(),
        messages,
        stream: false,
        tools,
        tool_choice: None,
    };

    // ── Call the API ───────────────────────────────────────────────────────────

    let url = format!("{}/v1/chat/completions", config.base_url.trim_end_matches('/'));

    let client = Client::new();
    let mut builder = client.post(&url);

    if !config.api_key.is_empty() {
        builder = builder.bearer_auth(&config.api_key);
    }
    for (name, value) in &config.extra_headers {
        if let (Ok(n), Ok(v)) = (
            http::header::HeaderName::from_bytes(name.as_bytes()),
            http::header::HeaderValue::from_str(value),
        ) {
            builder = builder.header(n, v);
        }
    }

    let response = builder.json(&oai_request).send().await.map_err(|e| {
        AIApiError::Transport(e)
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AIApiError::ErrorStatus(status, body));
    }

    let completion: openai::ChatCompletion = response.json().await.map_err(|e| {
        AIApiError::Transport(e)
    })?;

    // ── Build response events ──────────────────────────────────────────────────

    let request_id = Uuid::new_v4().to_string();
    let run_id = Uuid::new_v4().to_string();
    let msg_id = Uuid::new_v4().to_string();

    let mut events: Vec<Event> = Vec::new();

    // 1. Stream init
    events.push(Ok(api::ResponseEvent {
        r#type: Some(api::response_event::Type::Init(
            api::response_event::StreamInit {
                conversation_id: conversation_id.clone(),
                request_id: request_id.clone(),
                run_id: run_id.clone(),
            },
        )),
    }));

    // 2. Create root task on first turn
    if needs_create_task {
        events.push(Ok(api::ResponseEvent {
            r#type: Some(api::response_event::Type::ClientActions(
                api::response_event::ClientActions {
                    actions: vec![api::ClientAction {
                        action: Some(api::client_action::Action::CreateTask(
                            api::client_action::CreateTask {
                                task: Some(api::Task {
                                    id: root_task_id.clone(),
                                    description: "Main task".to_string(),
                                    ..Default::default()
                                }),
                            },
                        )),
                    }],
                },
            )),
        }));
    }

    // 3. Echo back tool results so the client's task state stays in sync
    if !input_tool_results.is_empty() {
        events.push(Ok(api::ResponseEvent {
            r#type: Some(api::response_event::Type::ClientActions(
                api::response_event::ClientActions {
                    actions: vec![api::ClientAction {
                        action: Some(api::client_action::Action::AddMessagesToTask(
                            api::client_action::AddMessagesToTask {
                                task_id: root_task_id.clone(),
                                messages: input_tool_results,
                            },
                        )),
                    }],
                },
            )),
        }));
    }

    // 4. Begin transaction
    events.push(Ok(api::ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![api::ClientAction {
                    action: Some(api::client_action::Action::BeginTransaction(
                        api::client_action::BeginTransaction {},
                    )),
                }],
            },
        )),
    }));

    // 5. Process the completion choice
    if let Some(choice) = completion.choices.into_iter().next() {
        let msg = choice.message;

        if let Some(tool_calls) = msg.tool_calls {
            // LLM wants to call tools → emit ToolCall messages
            let tool_call_messages: Vec<api::Message> = tool_calls
                .iter()
                .filter_map(|tc| translate::openai_tc_to_warp_tool_call(tc, &root_task_id))
                .collect();

            if !tool_call_messages.is_empty() {
                events.push(Ok(api::ResponseEvent {
                    r#type: Some(api::response_event::Type::ClientActions(
                        api::response_event::ClientActions {
                            actions: vec![api::ClientAction {
                                action: Some(api::client_action::Action::AddMessagesToTask(
                                    api::client_action::AddMessagesToTask {
                                        task_id: root_task_id.clone(),
                                        messages: tool_call_messages,
                                    },
                                )),
                            }],
                        },
                    )),
                }));
            }
        } else {
            // Text response → emit AgentOutput message
            let text = msg.content.unwrap_or_default();
            events.push(Ok(api::ResponseEvent {
                r#type: Some(api::response_event::Type::ClientActions(
                    api::response_event::ClientActions {
                        actions: vec![api::ClientAction {
                            action: Some(api::client_action::Action::AddMessagesToTask(
                                api::client_action::AddMessagesToTask {
                                    task_id: root_task_id.clone(),
                                    messages: vec![api::Message {
                                        id: msg_id.clone(),
                                        task_id: root_task_id.clone(),
                                        message: Some(api::message::Message::AgentOutput(
                                            api::message::AgentOutput { text },
                                        )),
                                        ..Default::default()
                                    }],
                                },
                            )),
                        }],
                    },
                )),
            }));
        }
    }

    // 6. Commit transaction
    events.push(Ok(api::ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![api::ClientAction {
                    action: Some(api::client_action::Action::CommitTransaction(
                        api::client_action::CommitTransaction {},
                    )),
                }],
            },
        )),
    }));

    // 7. Finished
    events.push(Ok(api::ResponseEvent {
        r#type: Some(api::response_event::Type::Finished(
            api::response_event::StreamFinished {
                reason: Some(api::response_event::stream_finished::Reason::Done(
                    api::response_event::stream_finished::Done {},
                )),
                ..Default::default()
            },
        )),
    }));

    Ok(events)
}

/// Collect tool results from the new request input so we can echo them back as
/// task messages (keeping the client's task state consistent with what the model sees).
#[cfg(not(target_family = "wasm"))]
fn collect_input_tool_results(request: &api::Request, task_id: &str) -> Vec<api::Message> {
    let Some(input) = &request.input else {
        return vec![];
    };
    let Some(api::request::input::Type::UserInputs(user_inputs)) = &input.r#type else {
        return vec![];
    };

    user_inputs
        .inputs
        .iter()
        .filter_map(|ui| {
            let api::request::input::user_inputs::user_input::Input::ToolCallResult(tcr) =
                ui.input.as_ref()?
            else {
                return None;
            };

            Some(api::Message {
                id: Uuid::new_v4().to_string(),
                task_id: task_id.to_string(),
                message: Some(api::message::Message::ToolCallResult(
                    api::message::ToolCallResult {
                        tool_call_id: tcr.tool_call_id.clone(),
                        result: convert_input_tcr_result(&tcr.result),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
        })
        .collect()
}

/// Convert an input `ToolCallResult.result` (from `request.proto`) to a
/// message-level `ToolCallResult.result` (from `task.proto`).
/// They reference the same underlying result types so we can move the values.
#[cfg(not(target_family = "wasm"))]
fn convert_input_tcr_result(
    result: &Option<api::request::input::tool_call_result::Result>,
) -> Option<api::message::tool_call_result::Result> {
    use api::message::tool_call_result::Result as MR;
    use api::request::input::tool_call_result::Result as IR;

    match result.as_ref()? {
        IR::RunShellCommand(r) => Some(MR::RunShellCommand(r.clone())),
        IR::ReadFiles(r) => Some(MR::ReadFiles(r.clone())),
        IR::ApplyFileDiffs(r) => Some(MR::ApplyFileDiffs(r.clone())),
        IR::Grep(r) => Some(MR::Grep(r.clone())),
        IR::FileGlobV2(r) => Some(MR::FileGlobV2(r.clone())),
        IR::SearchCodebase(r) => Some(MR::SearchCodebase(r.clone())),
        _ => None,
    }
}

// WASM stub: local proxy is desktop-only.
#[cfg(target_family = "wasm")]
pub fn run_direct_inference(
    _config: ai::providers::DirectProviderConfig,
    _request: api::Request,
) -> ResponseStream {
    use futures::stream;
    Box::pin(stream::empty())
}
