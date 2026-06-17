use warp_multi_agent_api as api;

use super::openai::{AssistantToolCall, ChatMessage, FunctionCallContent};

const SYSTEM_PROMPT: &str = "\
You are an expert AI assistant embedded in the Warp terminal. \
You help users with shell commands, coding, file editing, and system administration. \
You have tools to run shell commands, read and edit files, search code, \
and list files by pattern. \
When making code changes, prefer apply_file_diffs for accuracy. \
Be concise and take action rather than explaining what you're about to do.";

/// Build an OpenAI messages array from a Warp proto request.
///
/// The conversation history lives in `task_context.tasks[0].messages`.
/// The new turn's inputs live in `request.input.user_inputs.inputs`.
pub fn build_openai_messages(request: &api::Request) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    messages.push(ChatMessage {
        role: "system".into(),
        content: Some(SYSTEM_PROMPT.into()),
        tool_calls: None,
        tool_call_id: None,
    });

    // Replay history from the root task's message log.
    if let Some(task_ctx) = &request.task_context {
        if let Some(root_task) = task_ctx.tasks.first() {
            for msg in &root_task.messages {
                for oai_msg in warp_message_to_openai(msg) {
                    messages.push(oai_msg);
                }
            }
        }
    }

    // Append the new inputs for this turn.
    if let Some(input) = &request.input {
        if let Some(api::request::input::Type::UserInputs(user_inputs)) = &input.r#type {
            for user_input in &user_inputs.inputs {
                match &user_input.input {
                    Some(api::request::input::user_inputs::user_input::Input::UserQuery(q)) => {
                        messages.push(ChatMessage {
                            role: "user".into(),
                            content: Some(q.query.clone()),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }
                    Some(api::request::input::user_inputs::user_input::Input::ToolCallResult(
                        r,
                    )) => {
                        if let Some(oai_msg) = input_tool_result_to_openai(r) {
                            messages.push(oai_msg);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    messages
}

/// Convert a `Message` from the task history to zero or more OpenAI messages.
/// Returns a `Vec` because a single Warp message may need to be skipped or
/// may produce one message.
fn warp_message_to_openai(msg: &api::Message) -> Vec<ChatMessage> {
    match &msg.message {
        Some(api::message::Message::UserQuery(q)) => vec![ChatMessage {
            role: "user".into(),
            content: Some(q.query.clone()),
            tool_calls: None,
            tool_call_id: None,
        }],
        Some(api::message::Message::AgentOutput(a)) if !a.text.is_empty() => {
            vec![ChatMessage {
                role: "assistant".into(),
                content: Some(a.text.clone()),
                tool_calls: None,
                tool_call_id: None,
            }]
        }
        Some(api::message::Message::ToolCall(tc)) => {
            // An assistant tool call: present as a message with tool_calls.
            let Some(oai_tc) = warp_tool_call_to_openai_tc(tc) else {
                return vec![];
            };
            vec![ChatMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![oai_tc]),
                tool_call_id: None,
            }]
        }
        Some(api::message::Message::ToolCallResult(tcr)) => {
            // A tool result: present as a "tool" role message.
            history_tool_result_to_openai(tcr).into_iter().collect()
        }
        Some(api::message::Message::SystemQuery(sq)) => {
            // Surface system queries (e.g. auto code diff) as user messages.
            match &sq.r#type {
                Some(api::message::system_query::Type::AutoCodeDiff(acd)) => {
                    vec![ChatMessage {
                        role: "user".into(),
                        content: Some(acd.query.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    }]
                }
                _ => vec![],
            }
        }
        _ => vec![], // skip all other message types
    }
}

/// Convert a Warp `ToolCall` (from the conversation history) to an OpenAI
/// `AssistantToolCall`.
pub fn warp_tool_call_to_openai_tc(tc: &api::message::ToolCall) -> Option<AssistantToolCall> {
    use api::message::tool_call::Tool;

    let (name, args) = match &tc.tool {
        Some(Tool::RunShellCommand(r)) => (
            "run_shell_command",
            serde_json::json!({
                "command": r.command,
                "is_read_only": r.is_read_only,
                "uses_pager": r.uses_pager,
            }),
        ),
        Some(Tool::ReadFiles(rf)) => {
            let files: Vec<_> = rf
                .files
                .iter()
                .map(|f| serde_json::json!({"name": f.name}))
                .collect();
            ("read_files", serde_json::json!({"files": files}))
        }
        Some(Tool::ApplyFileDiffs(ad)) => {
            let diffs: Vec<_> = ad
                .diffs
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "file_path": d.file_path,
                        "search": d.search,
                        "replace": d.replace,
                    })
                })
                .collect();
            let new_files: Vec<_> = ad
                .new_files
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "file_path": f.file_path,
                        "content": f.content,
                    })
                })
                .collect();
            (
                "apply_file_diffs",
                serde_json::json!({
                    "summary": ad.summary,
                    "diffs": diffs,
                    "new_files": new_files,
                }),
            )
        }
        Some(Tool::Grep(g)) => (
            "grep",
            serde_json::json!({"queries": g.queries, "path": g.path}),
        ),
        Some(Tool::FileGlobV2(fg)) => (
            "file_glob_v2",
            serde_json::json!({
                "patterns": fg.patterns,
                "search_dir": fg.search_dir,
                "max_matches": fg.max_matches,
                "max_depth": fg.max_depth,
            }),
        ),
        Some(Tool::SearchCodebase(sc)) => (
            "search_codebase",
            serde_json::json!({
                "query": sc.query,
                "path_filters": sc.path_filters,
            }),
        ),
        _ => return None,
    };

    Some(AssistantToolCall {
        id: tc.tool_call_id.clone(),
        call_type: "function".into(),
        function: FunctionCallContent {
            name: name.to_string(),
            arguments: args.to_string(),
        },
    })
}

/// Convert a tool result *in the conversation history* to an OpenAI tool message.
fn history_tool_result_to_openai(tcr: &api::message::ToolCallResult) -> Option<ChatMessage> {
    use api::message::tool_call_result::Result as R;

    let content = match tcr.result.as_ref() {
        Some(R::RunShellCommand(r)) => format_shell_result(r),
        Some(R::ReadFiles(r)) => format_read_files_result(r),
        Some(R::ApplyFileDiffs(r)) => format_apply_diffs_result(r),
        Some(R::Grep(r)) => format_grep_result(r),
        Some(R::FileGlobV2(r)) => format_file_glob_v2_result(r),
        Some(R::SearchCodebase(r)) => format_search_codebase_result(r),
        _ => return None,
    };

    Some(ChatMessage {
        role: "tool".into(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(tcr.tool_call_id.clone()),
    })
}

/// Convert a tool result from the *new request input* to an OpenAI tool message.
pub fn input_tool_result_to_openai(
    tcr: &api::request::input::ToolCallResult,
) -> Option<ChatMessage> {
    use api::request::input::tool_call_result::Result as R;

    let content = match tcr.result.as_ref() {
        Some(R::RunShellCommand(r)) => format_shell_result(r),
        Some(R::ReadFiles(r)) => format_read_files_result(r),
        Some(R::ApplyFileDiffs(r)) => format_apply_diffs_result(r),
        Some(R::Grep(r)) => format_grep_result(r),
        Some(R::FileGlobV2(r)) => format_file_glob_v2_result(r),
        Some(R::SearchCodebase(r)) => format_search_codebase_result(r),
        _ => return None,
    };

    Some(ChatMessage {
        role: "tool".into(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(tcr.tool_call_id.clone()),
    })
}

// ── Warp proto → OpenAI JSON tool call arguments ──────────────────────────────

/// Parse an OpenAI tool call and build the corresponding Warp `ToolCall` proto.
pub fn openai_tc_to_warp_tool_call(tc: &AssistantToolCall, task_id: &str) -> Option<api::Message> {
    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).ok()?;
    use api::message::tool_call as tc_types;

    let tool = match tc.function.name.as_str() {
        "run_shell_command" => tc_types::Tool::RunShellCommand(tc_types::RunShellCommand {
            command: args["command"].as_str()?.to_string(),
            is_read_only: args["is_read_only"].as_bool().unwrap_or(false),
            uses_pager: args["uses_pager"].as_bool().unwrap_or(false),
            ..Default::default()
        }),
        "read_files" => {
            let files = args["files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| {
                            Some(tc_types::read_files::File {
                                name: f["name"].as_str()?.to_string(),
                                line_ranges: vec![],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            tc_types::Tool::ReadFiles(tc_types::ReadFiles { files })
        }
        "apply_file_diffs" => {
            let diffs = args["diffs"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            Some(tc_types::apply_file_diffs::FileDiff {
                                file_path: d["file_path"].as_str()?.to_string(),
                                search: d["search"].as_str().unwrap_or("").to_string(),
                                replace: d["replace"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let new_files = args["new_files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| {
                            Some(tc_types::apply_file_diffs::NewFile {
                                file_path: f["file_path"].as_str()?.to_string(),
                                content: f["content"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let deleted_files = args["deleted_files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| {
                            Some(tc_types::apply_file_diffs::DeleteFile {
                                file_path: f["file_path"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            tc_types::Tool::ApplyFileDiffs(tc_types::ApplyFileDiffs {
                summary: args["summary"].as_str().unwrap_or("").to_string(),
                diffs,
                new_files,
                deleted_files,
                ..Default::default()
            })
        }
        "grep" => {
            let queries = args["queries"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|q| q.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            tc_types::Tool::Grep(tc_types::Grep {
                queries,
                path: args["path"].as_str().unwrap_or("").to_string(),
            })
        }
        "file_glob_v2" => {
            let patterns = args["patterns"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            tc_types::Tool::FileGlobV2(tc_types::FileGlobV2 {
                patterns,
                search_dir: args["search_dir"].as_str().unwrap_or("").to_string(),
                max_matches: args["max_matches"].as_i64().unwrap_or(0) as i32,
                max_depth: args["max_depth"].as_i64().unwrap_or(0) as i32,
                min_depth: 0,
            })
        }
        "search_codebase" => {
            let path_filters = args["path_filters"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            tc_types::Tool::SearchCodebase(tc_types::SearchCodebase {
                query: args["query"].as_str()?.to_string(),
                path_filters,
                codebase_path: args["codebase_path"].as_str().unwrap_or("").to_string(),
            })
        }
        _ => return None,
    };

    Some(api::Message {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: tc.id.clone(),
            tool: Some(tool),
        })),
        ..Default::default()
    })
}

// ── Tool result formatters ────────────────────────────────────────────────────

fn format_shell_result(r: &api::RunShellCommandResult) -> String {
    use api::run_shell_command_result::Result as R;
    match &r.result {
        Some(R::CommandFinished(f)) => {
            format!(
                "Command: {}\nExit code: {}\n{}",
                r.command, f.exit_code, f.output
            )
        }
        Some(R::LongRunningCommandSnapshot(s)) => {
            format!("Command: {} (still running)\n{}", r.command, s.output)
        }
        Some(R::PermissionDenied(_)) => {
            format!("Command denied: {}", r.command)
        }
        None => format!("Command: {} (no result)", r.command),
    }
}

fn format_read_files_result(r: &api::ReadFilesResult) -> String {
    use api::read_files_result::Result as R;
    match &r.result {
        Some(R::TextFilesSuccess(s)) => s
            .files
            .iter()
            .map(|f| format!("=== {} ===\n{}", f.file_path, f.content))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(R::AnyFilesSuccess(s)) => {
            format!("Read {} file(s)", s.files.len())
        }
        Some(R::Error(e)) => format!("Error reading files: {}", e.message),
        None => "No result".to_string(),
    }
}

fn format_apply_diffs_result(r: &api::ApplyFileDiffsResult) -> String {
    use api::apply_file_diffs_result::Result as R;
    match &r.result {
        Some(R::Success(s)) => {
            let files: Vec<_> = s
                .updated_files_v2
                .iter()
                .filter_map(|u| u.file.as_ref().map(|f| f.file_path.as_str()))
                .collect();
            format!("Successfully updated: {}", files.join(", "))
        }
        Some(R::Error(e)) => format!("Error applying diffs: {}", e.message),
        None => "No result".to_string(),
    }
}

fn format_grep_result(r: &api::GrepResult) -> String {
    use api::grep_result::Result as R;
    match &r.result {
        Some(R::Success(s)) => {
            let mut lines = Vec::new();
            for file_match in &s.matched_files {
                let line_nums: Vec<_> = file_match
                    .matched_lines
                    .iter()
                    .map(|l| l.line_number.to_string())
                    .collect();
                lines.push(format!(
                    "{}: lines {}",
                    file_match.file_path,
                    line_nums.join(", ")
                ));
            }
            if lines.is_empty() {
                "No matches found".to_string()
            } else {
                lines.join("\n")
            }
        }
        Some(R::Error(e)) => format!("Grep error: {}", e.message),
        None => "No result".to_string(),
    }
}

fn format_file_glob_v2_result(r: &api::FileGlobV2Result) -> String {
    use api::file_glob_v2_result::Result as R;
    match &r.result {
        Some(R::Success(s)) => {
            let files: Vec<_> = s
                .matched_files
                .iter()
                .map(|f| f.file_path.as_str())
                .collect();
            if files.is_empty() {
                "No files matched".to_string()
            } else {
                files.join("\n")
            }
        }
        Some(R::Error(e)) => format!("Glob error: {}", e.message),
        None => "No result".to_string(),
    }
}

fn format_search_codebase_result(r: &api::SearchCodebaseResult) -> String {
    use api::search_codebase_result::Result as R;
    match &r.result {
        Some(R::Success(s)) => s
            .files
            .iter()
            .map(|f| format!("=== {} ===\n{}", f.file_path, f.content))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(R::Error(e)) => format!("Search error: {}", e.message),
        None => "No result".to_string(),
    }
}
