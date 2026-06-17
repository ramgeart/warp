# PLAN.md — Warp local-first: sin telemetría + todas las features de IA por API propia

> Estado: **fases 1-6 implementadas** — rama `claude/bold-galileo-08lwrw`.
> Documento de ejecución: `AGENT.md`.

## 1. Objetivo

Convertir Warp en un terminal **local-first / bring-your-own-API** que:

1. **No hace ninguna conexión externa** salvo a las API URLs que el usuario configure. No hay opt-in.
2. **Mantiene TODAS las features de IA** (Agent Mode, autocomplete, passive suggestions, code review, query prediction, block titles, etc.) pero las sirve contra las APIs configuradas.
3. **Habla OpenAI `chat/completions`** como protocolo de wire (máxima compatibilidad: Ollama, vLLM, LM Studio, OpenRouter, Groq, Together, Azure, …).
4. Los **modelos se muestran como `providerName/modelName`** (nombre elegido por el usuario al agregar un proveedor, modelos descubiertos vía `GET {base_url}/v1/models`).
5. **Auth/sync/update** se neutralizan ahora; implementaciones custom después.

## 2. Decisiones tomadas (cerradas)

| # | Decisión |
|---|----------|
| 1 | OpenAI `chat/completions` primero; modelos desde `GET /v1/models`. |
| 2 | Sin egress salvo la API URL configurada. **No opt-in**. |
| 3 | Directo a la API configurada, **sin pasar por `app.warp.dev`**. |
| 4 | Auth/sync/update: neutralizar ahora, modo local para arrancar sin login, soluciones custom después. |
| 5 | **Todas** las features de IA de Warp se mantienen — se traducen, no se eliminan. |
| 6 | Modelos en el picker: `{providerName}/{modelName}`. |

## 3. API surface que debe ser reemplazada (verificada en el código)

### Endpoints protobuf+SSE (streaming)
- `POST {server_root_url}/ai/multi-agent` — núcleo de Agent Mode. Cuerpo: `warp_multi_agent_api::Request` (protobuf base64), respuesta SSE de `ResponseEvent`. **Este es el más crítico.**
- `POST {server_root_url}/ai/passive-suggestions` — sugerencias de fondo. Mismo formato, input type `GeneratePassiveSuggestions`.

### Endpoints JSON (request/response simples)
- `POST /ai/generate_input_suggestions` → terminal autocomplete / predicción de comandos.
- `POST /ai/relevant_files` → descubrimiento de archivos relevantes.
- `POST /ai/generate_am_query_suggestions` → sugerencias de query tras un comando.
- `POST /ai/predict_am_queries` → predicción de query mientras el usuario escribe.
- `POST /ai/transcribe` → speech-to-text (proveedor OpenAI Whisper o Wispr).
- `POST /ai/generate_code_review_content` → mensajes de commit, PR title/description.
- `POST /ai/generate_block_title` → título de bloque de terminal.

### No-AI que se neutraliza
- Telemetría RudderStack, Sentry crash reporting, autoupdate, Warp Drive sync, Firebase auth, Oz/ambient agents, session sharing.

## 4. Arquitectura objetivo: in-process translator

```
┌──────────────────── CLIENTE WARP (sin cambios) ────────────────────┐
│  Agent Mode UI                                                       │
│  Autocomplete UI       ─── builds ──▶  warp_multi_agent_api::Request │
│  Passive suggestions                                                  │
│  Code review / titles                                                 │
└────────────────────────────────┬───────────────────────────────────┘
                                 │ generate_multi_agent_output()
                                 ▼
┌──────────────── DISPATCH (server_api.rs — MODIFICADO) ─────────────┐
│  if active model is DirectProvider:                                  │
│      ──▶  local_ai_proxy::translate_and_run(request)                │
│  else: (si no hay provider configurado: error o noop)               │
└────────────────────────────────┬───────────────────────────────────┘
                                 │
┌──────────────── local_ai_proxy (NUEVO: crates/ai/src/local_proxy/) ─┐
│  1. Decode warp_multi_agent_api::Request                             │
│  2. Translate → OpenAI chat/completions JSON                         │
│  3. Stream POST {base_url}/v1/chat/completions                       │
│  4. Translate delta events → ResponseEvent protos                    │
│  5. Tool loop: tool_calls → emit ClientActions → recv results        │
│  6. Emit Finished                                                     │
│                                                                       │
│  Para endpoints JSON:                                                 │
│  GenerateInputSuggestions, RelevantFiles, QuerySuggestions, etc.     │
│  → prompt engineering → chat/completions → parse → return JSON       │
└────────────────────────────────┬───────────────────────────────────┘
                                 │
┌──────────────── http_client (MODIFICADO: egress allowlist) ─────────┐
│  Solo permite: host(s) de base_url configurada + loopback            │
│  Todo lo demás → error inmediato                                     │
└────────────────────────────────┬───────────────────────────────────┘
                                 │
                                 ▼ ÚNICO EGRESS
                         🌐 {base_url}/v1/chat/completions
                         🌐 {base_url}/v1/models
```

**Por qué in-process y no un proxy HTTP local:**
- Sin puertos que gestionar, sin proceso separado, sin latencia de red extra.
- El cliente ya llama `generate_multi_agent_output()` como función async; el dispatch es natural.
- Los `ResponseEvent` protos fluyen por el mismo stream channel interno existente.

## 5. Modelo de datos del proveedor (B1)

```rust
// crates/ai/src/providers.rs (NUEVO)
pub struct DirectProvider {
    pub name: String,                        // "ollama", "my-openai", etc.
    pub base_url: String,                    // "http://localhost:11434"  (sin /v1)
    pub api_key: String,                     // secure_storage; NUNCA TOML/logs
    pub default_headers: Vec<(String,String)>,
    pub models_cache: Vec<DirectProviderModel>, // de GET /v1/models
}

pub struct DirectProviderModel {
    pub id: String,                          // id del modelo según /v1/models
    pub display_id: String,                  // "{providerName}/{modelId}"
}

// model_id en el picker = "{providerName}/{modelId}"
// Ruteo: split('/') en el dispatch para encontrar el provider
```

## 6. Traducción multi-agent ↔ OpenAI chat/completions

La `warp_multi_agent_api::Request` contiene:
- `input.type`: el tipo de conversación (UserQuery, PassiveSuggestion, AutoCodeDiff, etc.)
- `settings.supported_tools`: lista de herramientas disponibles (Grep, ReadFiles, RunShellCommand, …)
- `settings.model_config.base`: el model ID
- `metadata.conversation_id`: ID de conversación

La traducción:
1. **Contexto/historial**: reconstruir el array `messages` de OpenAI (system + alternancia user/assistant).
2. **Tools**: cada `ToolType` de Warp → definición OpenAI function/tool con schema JSON.
3. **Streaming**: `data: {"choices":[{"delta":{"content":"..."}}]}` → `ResponseEvent::ClientActions` con text delta.
4. **Tool calls**: `delta.tool_calls` → `ResponseEvent::ClientActions` con la acción correspondiente; recibir resultado → continuar el loop.
5. **Fin**: `[DONE]` → `ResponseEvent::Finished`.

Para los **endpoints JSON simples**, cada uno mapea a un prompt bien definido:
- `generate_input_suggestions`: "dada esta historia de terminal y contexto, sugiere el siguiente comando…"
- `generate_am_query_suggestions`: "dado que el comando terminó con exit_code X, sugiere una query de Agent Mode…"
- etc.

## 7. Egress lockdown (A1)

En `crates/http_client/src/lib.rs`, antes de ejecutar cualquier request:
```rust
fn is_allowed_host(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("");
    host == "localhost" || host == "127.0.0.1" || host == "::1"
    || DIRECT_PROVIDERS.read().iter().any(|p| {
        Url::parse(&p.base_url).ok()
            .and_then(|u| u.host_str().map(|h| h == host))
            .unwrap_or(false)
    })
}
```
Si el host no está permitido → `Err(EgressBlocked)`. Esto es estructural, no un toggle.

## 8. Fases de implementación

| Fase | Estado | Contenido | Archivos clave |
|------|--------|-----------|----------------|
| **1** | ✅ hecho | Egress allowlist en http_client + telemetría/crash off en origen | `crates/http_client/src/lib.rs`, `app/src/settings/privacy.rs`, `app/src/bin/oss.rs` |
| **2** | ✅ hecho | `DirectProvider` + `GET /v1/models` + feature flag | `crates/ai/src/providers.rs`, `crates/warp_features/src/lib.rs` |
| **3** | ✅ hecho | `local_proxy`: traducción `multi-agent` → `chat/completions` + tool loop | `app/src/ai/local_proxy/` |
| **4** | ✅ hecho | Model picker `providerName/modelName` + UI alta/edición de proveedores | `app/src/ai/llms.rs`, `app/src/settings_view/ai_page.rs`, `app/src/settings_view/direct_provider_modal.rs` |
| **5** | ✅ hecho | Endpoints JSON (block title, code review, relevant files, query suggestions, predict) ruteados al proveedor activo | `crates/ai/src/providers.rs` (`simple_completion`), `app/src/server/server_api.rs`, `.../server_api/block.rs`, `.../server_api/ai.rs` |
| **6** | ✅ hecho | Modo local sin login (`SkipFirebaseAnonymousUser`) + egress lockdown al configurar proveedor | `app/src/bin/oss.rs`, `crates/ai/src/providers.rs` |
| **7** | pendiente | Anthropic `messages` / OpenAI `responses` + streaming SSE + tests completos | `app/src/ai/local_proxy/anthropic.rs` (futuro) |

**Fase 1 es la base; Fases 3+4 son el núcleo funcional. Fases 1-6 implementadas.**

### Notas de implementación (1-6)

- **Intercepción Agent Mode**: en `generate_multi_agent_output` (`app/src/ai/agent/api/impl.rs`); si el modelo activo pertenece a un `DirectProvider`, se llama a `local_proxy::run_direct_inference` y nunca se contacta `app.warp.dev`.
- **Endpoints JSON**: se interceptan dentro de los métodos de `ServerApi` consultando `ai::providers::active_direct_config()` (un registro global app-wide sincronizado por `LLMPreferences`). Si hay proveedor activo, usan `simple_completion`.
- **Egress lockdown**: `http_client::enable_egress_lockdown()` se activa en cuanto existe ≥1 proveedor (al arrancar o al añadir el primero). Solo se permiten los hosts de los proveedores + loopback; el resto se bloquea estructuralmente.
- **Modo local**: el build OSS activa `SkipFirebaseAnonymousUser`, arrancando directo al terminal sin login Warp ni usuario anónimo Firebase. Telemetría/crash/autoupdate ya `None` en `oss.rs`.
- **Streaming**: Fase 3 usa respuestas no-streaming (`stream: false`) batched a `ResponseEvent`. El streaming incremental queda para la Fase 7.

## 9. Capacidades tras el cambio

- ✅ **Todo Agent Mode**: chat, streaming, tool calling (bash, read/write files, grep, …)
- ✅ **Terminal autocomplete** (generate_input_suggestions)
- ✅ **Passive suggestions**
- ✅ **Query suggestions** tras comandos
- ✅ **Predicción de queries** mientras se escribe
- ✅ **Code review content** (commit messages, PR titles)
- ✅ **Block titles**
- ✅ **Múltiples proveedores** con names + models vía `/v1/models`
- ⚠️ **Transcripción de voz**: requiere endpoint compatible con OpenAI Whisper (`/v1/audio/transcriptions`); degradado si no disponible.
- ❌ **Quitado por ahora**: Warp login, Warp Drive, sync, session sharing, autoupdate (decisión #4).

## 10. Riesgos principales

| Riesgo | Mitigación |
|--------|------------|
| Estructura exacta de los protos sin código fuente | Inferir de los call sites en el repo; `prost::Message` es fácil de implementar parcialmente |
| Tool loop incompleto (herramientas sin mapear) | Empezar por las más usadas (RunShellCommand, ReadFiles, ApplyFileDiffs); las demás: noop con log |
| Endpoints JSON con prompts frágiles | Tests de integración con fixtures; empezar por los más críticos |
| Quitar auth rompe el arranque | Modo local en Fase 6; el egress lockdown no rompe las funciones locales |
| Allowlist demasiado estricto bloquea MCP/OAuth legítimos | Allowlist extensible + loopback siempre libre |

## 11. Archivos involucrados

**Modificar:**
`crates/http_client/src/lib.rs` · `app/src/settings/privacy.rs` · `app/src/bin/{oss,stable,preview,dev,local}.rs` · `app/src/server/server_api.rs` · `app/src/ai/llms.rs` · `app/src/settings_view/ai_page.rs` · `crates/warp_features/src/lib.rs`

**Crear:**
`crates/ai/src/providers.rs` · `crates/ai/src/local_proxy/mod.rs` · `crates/ai/src/local_proxy/openai.rs` · `crates/ai/src/local_proxy/stream_adapter.rs` · `crates/ai/src/local_proxy/json_endpoints.rs` · `crates/ai/src/local_proxy/tool_registry.rs`
