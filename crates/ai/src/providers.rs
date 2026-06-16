/// Direct Inference Providers — AI API endpoints the client calls directly,
/// bypassing the Warp backend (`app.warp.dev`).
///
/// Each provider is an OpenAI-compatible endpoint.  Models are discovered via
/// `GET {base_url}/v1/models` and presented in the UI as `{name}/{model_id}`.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warpui_core::{Entity, ModelContext, SingletonEntity};
use warpui_extras::secure_storage::{self, AppContextExt};

/// Secure storage key for the full list of `DirectProvider` (including keys).
pub const DIRECT_PROVIDERS_STORAGE_KEY: &str = "DirectProviders";

// ── Data types ────────────────────────────────────────────────────────────────

/// A user-configured AI provider that the client calls directly via the
/// OpenAI chat completions API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectProvider {
    /// Stable UUID used for routing and as a storage key.
    pub id: String,
    /// Human-readable name shown in the UI, e.g. "ollama" or "my-openai".
    pub name: String,
    /// Base URL **without** a trailing path, e.g. `http://localhost:11434`.
    /// The client appends `/v1/models` and `/v1/chat/completions` as needed.
    pub base_url: String,
    /// API key — stored in secure storage; never written to TOML or logs.
    pub api_key: String,
    /// Extra HTTP headers to attach to every request (e.g. `anthropic-version`).
    pub default_headers: Vec<(String, String)>,
    /// Models fetched from `GET {base_url}/v1/models` or entered manually.
    pub models: Vec<DirectProviderModel>,
}

impl Default for DirectProvider {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            default_headers: Vec::new(),
            models: Vec::new(),
        }
    }
}

impl DirectProvider {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            base_url: base_url.into(),
            ..Default::default()
        }
    }

    pub fn chat_completions_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/v1/chat/completions", base)
    }

    pub fn models_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/v1/models", base)
    }
}

/// A single model offered by a `DirectProvider`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DirectProviderModel {
    /// Raw model ID returned by `/v1/models`, e.g. `"llama3.2"`.
    pub id: String,
    /// Stable UUID used as the `ModelConfig.base` key for routing.
    pub config_key: String,
}

impl DirectProviderModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            config_key: Uuid::new_v4().to_string(),
        }
    }

    /// Returns the picker label: `"{provider_name}/{model_id}"`.
    pub fn display_id<'a>(&'a self, provider_name: &'a str) -> String {
        format!("{}/{}", provider_name, self.id)
    }
}

// ── Singleton model ───────────────────────────────────────────────────────────

/// Singleton that manages the list of `DirectProvider`s for the current user.
pub struct DirectProviderManager {
    providers: Vec<DirectProvider>,
}

impl Entity for DirectProviderManager {
    type Event = DirectProviderManagerEvent;
}
impl SingletonEntity for DirectProviderManager {}

#[derive(Debug, Clone)]
pub enum DirectProviderManagerEvent {
    ProvidersUpdated,
}

impl DirectProviderManager {
    /// Construct the singleton; pass to `ctx.add_singleton_model`.
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let providers = Self::load_from_storage(ctx);
        // Register each provider's host with the egress allowlist so that
        // http_client permits outbound connections to it.
        for provider in &providers {
            register_provider_with_egress_allowlist(provider);
        }
        Self { providers }
    }

    pub fn providers(&self) -> &[DirectProvider] {
        &self.providers
    }

    pub fn find_by_config_key(&self, config_key: &str) -> Option<(&DirectProvider, &DirectProviderModel)> {
        for provider in &self.providers {
            for model in &provider.models {
                if model.config_key == config_key {
                    return Some((provider, model));
                }
            }
        }
        None
    }

    pub fn upsert_provider(
        &mut self,
        provider: DirectProvider,
        ctx: &mut ModelContext<Self>,
    ) {
        register_provider_with_egress_allowlist(&provider);
        if let Some(existing) = self.providers.iter_mut().find(|p| p.id == provider.id) {
            *existing = provider;
        } else {
            self.providers.push(provider);
        }
        self.save_to_storage(ctx);
        ctx.emit(DirectProviderManagerEvent::ProvidersUpdated);
        ctx.notify();
    }

    pub fn remove_provider(&mut self, id: &str, ctx: &mut ModelContext<Self>) {
        self.providers.retain(|p| p.id != id);
        self.save_to_storage(ctx);
        ctx.emit(DirectProviderManagerEvent::ProvidersUpdated);
        ctx.notify();
    }

    /// Update the model list for a provider (typically after a `/v1/models` fetch).
    pub fn set_models(
        &mut self,
        provider_id: &str,
        model_ids: Vec<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(provider) = self.providers.iter_mut().find(|p| p.id == provider_id) else {
            return;
        };
        // Preserve existing config_keys for models that are already known so
        // that stored model selections remain valid after a refresh.
        let existing: std::collections::HashMap<String, String> = provider
            .models
            .iter()
            .map(|m| (m.id.clone(), m.config_key.clone()))
            .collect();
        provider.models = model_ids
            .into_iter()
            .map(|id| {
                let config_key = existing
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                DirectProviderModel { id, config_key }
            })
            .collect();
        self.save_to_storage(ctx);
        ctx.emit(DirectProviderManagerEvent::ProvidersUpdated);
        ctx.notify();
    }

    fn load_from_storage(ctx: &mut ModelContext<Self>) -> Vec<DirectProvider> {
        let json = match ctx.secure_storage().read_value(DIRECT_PROVIDERS_STORAGE_KEY) {
            Ok(json) => json,
            Err(e) => {
                if !matches!(e, secure_storage::Error::NotFound) {
                    log::error!("Failed to read DirectProviders from secure storage: {e:#}");
                }
                return Vec::new();
            }
        };
        match serde_json::from_str(&json) {
            Ok(providers) => providers,
            Err(e) => {
                log::error!("Failed to deserialize DirectProviders: {e:#}");
                Vec::new()
            }
        }
    }

    fn save_to_storage(&self, ctx: &mut ModelContext<Self>) {
        let json = match serde_json::to_string(&self.providers) {
            Ok(j) => j,
            Err(e) => {
                log::error!("Failed to serialize DirectProviders: {e}");
                return;
            }
        };
        if let Err(e) = ctx.secure_storage().write_value(DIRECT_PROVIDERS_STORAGE_KEY, &json) {
            log::error!("Failed to save DirectProviders to secure storage: {e}");
        }
    }
}

/// Register the provider's host with `http_client`'s egress allowlist so that
/// outbound connections are permitted once lockdown is enabled.
fn register_provider_with_egress_allowlist(provider: &DirectProvider) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = provider.base_url.parse::<url::Url>() {
        if let Some(host) = url.host_str() {
            http_client::register_allowed_egress_host(host.to_owned());
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = provider;
}

// ── Model discovery ───────────────────────────────────────────────────────────

/// Response shape returned by `GET /v1/models` (OpenAI-compatible).
#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelObject>,
}

#[derive(Debug, Deserialize)]
struct ModelObject {
    id: String,
}

/// Fetch the list of model IDs from a provider's `/v1/models` endpoint.
///
/// Returns model IDs sorted alphabetically.  Logs errors and returns `None`
/// on failure so the caller can fall back to manual model entry.
#[cfg(not(target_family = "wasm"))]
pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
    extra_headers: &[(String, String)],
) -> anyhow::Result<Vec<String>> {
    use http_client::Client;

    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

    let client = Client::new();
    let mut builder = client.get(&url);

    if !api_key.is_empty() {
        builder = builder.bearer_auth(api_key);
    }

    for (name, value) in extra_headers {
        if let (Ok(n), Ok(v)) = (
            http::header::HeaderName::from_bytes(name.as_bytes()),
            http::header::HeaderValue::from_str(value),
        ) {
            builder = builder.header(n, v);
        }
    }

    let response = builder.send().await?;

    if !response.status().is_success() {
        anyhow::bail!(
            "GET {} returned status {}",
            url,
            response.status()
        );
    }

    let body: ModelsListResponse = response.json().await?;
    let mut ids: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
    ids.sort();
    Ok(ids)
}
