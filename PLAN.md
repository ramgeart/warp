# PLAN.md — Warp local-first: sin conexiones externas + IA por API propia (BaseURL)

> Estado: **propuesta de implementación** (no implementado todavía).
> Rama de trabajo: `claude/bold-galileo-08lwrw`.
> Documento complementario: ver `AGENT.md` para reglas de ejecución, convenciones del repo y *definition of done*.

## 1. Objetivo

Convertir Warp en un terminal **local-first / bring-your-own-API**:

1. **Eliminar toda conexión externa** del cliente (telemetría, crash reporting, autoupdate, sync, auth, Warp Drive, etc.). La **única** salida de red permitida es la(s) **URL de API de IA que el usuario configure**. **No es opt-in**: se bloquea estructuralmente, no por toggle.
2. **Hablar directo con la API de IA configurada** (sin pasar por `app.warp.dev`), indicando `BaseURL`:
   - **Primero OpenAI `chat/completions`** (`POST {base_url}/v1/chat/completions`) por ser el formato más compatible (Ollama, vLLM, OpenRouter, LM Studio, Groq, Together, Azure OpenAI…).
   - Los **modelos se descubren** vía `GET {base_url}/v1/models` (con *fallback* a entrada manual).
   - Anthropic `messages` y OpenAI `responses` quedan como fases posteriores.

## 2. Decisiones tomadas (cierran las preguntas abiertas del plan anterior)

| # | Decisión |
|---|----------|
| 1 | **Empezar por OpenAI `chat/completions`**; los modelos se toman de `GET /v1/models`. |
| 2 | **Quitar que requiera conexión externa**, excepto la API URL configurada. **No hay opt-in** a telemetría/servicios. |
| 3 | **Quitar el paso por `app.warp.dev`** para IA → contactar directo con las APIs especificadas. |
| 4 | **Update, sync, auth, etc.**: se neutralizan ahora (sin egress); las **soluciones custom** se implementan más adelante. |

**Interpretación explícita de la #4 (corregir si no es la intención):** como la #2 prohíbe egress no autorizado, los subsistemas de auth/sync/update **se desconectan ahora** y se añade un **modo local/offline** que permite arrancar la app **sin login de Warp**. Se dejan *seams* (puntos de extensión) para reimplementar esas funciones contra servicios propios después.

## 3. Realidad del código actual (verificada) que condiciona el diseño

- **Telemetría build-gated:** `crates/warp_core/src/channel/config.rs` → `telemetry_config: Option<…>` y `crash_reporting_config: Option<…>`. `app/src/bin/oss.rs` ya los pone en `None` (sin RudderStack/Sentry) y `ChannelState::is_telemetry_available()` oculta la UI.
- **Opt-out no absoluto:** `app/src/settings/privacy.rs::should_disable_telemetry()` puede ser anulado por `is_telemetry_force_enabled` y `FeatureFlag::AgentModeAnalytics`. → Hay que neutralizarlo (decisión #2).
- **Toda la IA pasa por el backend:** `app/src/server/server_api.rs::generate_multi_agent_output()` hace `POST {server_root_url}/ai/multi-agent` con **protobuf** (`warp_multi_agent_api::Request`) y respuesta SSE. El cliente **nunca** llama a `api.openai.com`. *BYOK* y *Custom Endpoints* (`crates/ai/src/api_keys.rs`) inyectan `api_key`/`base_url` **dentro** del protobuf → igual viajan a Warp. → Hay que **reemplazar** esta ruta (decisión #3).
- **Choke point de red:** todo el HTTP sale por `crates/http_client/src/lib.rs` (wrapper de `reqwest`). → Es el lugar ideal para imponer el **allowlist de egress** (decisión #2).
- **Override de servidor ya existe:** `WARP_SERVER_ROOT_URL` / `--server-root-url` → `ChannelState::override_server_root_url()`. Útil como mecanismo, no como solución (re-apunta el backend, no habla OpenAI).

## 4. Arquitectura objetivo

```
                 ┌─────────────────────────────────────────────┐
   Agent Mode ──▶│  ai::direct_provider (NUEVO)                 │
   (UI igual)    │   • OpenAI chat/completions (fase 1)         │──┐
                 │   • parser SSE + tool-loop en cliente        │  │
                 │   • GET /v1/models para el picker            │  │
                 └─────────────────────────────────────────────┘  │
                                                                   ▼
   ┌──────────────────────────────────────────────┐   SOLO destino permitido:
   │ http_client (choke point)                     │   {base_url} configurada
   │  EGRESS ALLOWLIST (NUEVO, no-opt-in):         │   + loopback/localhost
   │   permite únicamente host(s) de la API URL    │──────────────▶ 🌐 API IA
   │   + loopback. Bloquea TODO lo demás.          │
   └──────────────────────────────────────────────┘
        ▲              ▲                ▲
        │              │                │  (bloqueados)
   telemetría     app.warp.dev     autoupdate / sync / auth / Warp Drive
   (RudderStack)  (multi-agent)    → neutralizados + modo local
   (Sentry)       → reemplazado
```

## 5. Plan de trabajo

### PARTE A — Lockdown de egress (decisión #2, la garantía dura)

**A1. Allowlist en el choke point `http_client`.**
- En `crates/http_client/src/lib.rs`: antes de ejecutar cualquier request, validar el host destino contra un allowlist en memoria. Permitir **solo**: host(s) de la(s) `base_url` configurada(s) por el usuario + loopback (`127.0.0.1`, `localhost`, `::1`). Cualquier otro destino → error inmediato (no se envía).
- El allowlist se actualiza cuando el usuario cambia/agrega un `DirectProvider`.
- Esto hace el requisito **estructural y no-opt-in**: aunque quede algún *call site* legado, no podrá salir a la red.

**A2. Apagar telemetría y crash reporting en origen.**
- Build objetivo con `telemetry_config: None` y `crash_reporting_config: None` (patrón `app/src/bin/oss.rs`).
- `app/src/settings/privacy.rs`: `should_disable_telemetry()` → `true` incondicional; *defaults* en `false`; ignorar `is_telemetry_force_enabled` y `FeatureFlag::AgentModeAnalytics`.
- Hacer no-op el envío en `app/src/server/telemetry/{collector,macros}.rs` (early-return) para no depender solo del allowlist.

**A3. Documentar/validar que no queda egress.** Test que arranca la app y falla si el allowlist recibe cualquier host ≠ API URL/loopback.

### PARTE B — Cliente directo OpenAI (decisiones #1 y #3, el núcleo)

**B1. Modelo de datos.** En `crates/ai/src/` (módulo nuevo `providers.rs`):
```rust
pub struct DirectProvider {
    pub name: String,
    pub base_url: String,        // https://api.openai.com  | http://localhost:11434/v1
    pub api_key: String,         // secure_storage; NUNCA TOML/logs
    pub protocol: ApiProtocol,   // fase 1: OpenAiChatCompletions
    pub default_headers: Vec<(String, String)>,
    // modelos: descubiertos vía /v1/models; cache local + override manual
}
pub enum ApiProtocol { OpenAiChatCompletions, /* fase 2: OpenAiResponses, AnthropicMessages */ }
```

**B2. Descubrimiento de modelos.** `GET {base_url}/v1/models` → poblar el *picker* (`MODELS_BY_FEATURE_CACHE_KEY` en `app/src/ai/llms.rs`). *Fallback*: lista manual si el endpoint no existe (algunos proveedores locales).

**B3. Cliente `crates/ai/src/direct_provider/`.**
- `openai.rs`: arma el JSON de `chat/completions` (mensajes, `tools`, `stream: true`); parsea SSE (`data: {…}` *deltas*, `tool_calls`, `[DONE]`).
- `stream_adapter.rs`: mapea el *streaming* nativo a los eventos internos que la UI ya consume.
- **Tool-loop en cliente** (lo que hacía el servidor): mandar herramientas, recibir *tool calls*, ejecutarlas con el *runtime* de tools existente, reenviar resultados hasta el turno final.
- Reutilizar `crates/http_client` (`.post()`, `.eventsource()`, headers/`bearer_auth`).

**B4. Reemplazo del dispatch (quitar `app.warp.dev`).**
- Donde hoy se llama a `generate_multi_agent_output()` (`app/src/ai/agent/api.rs`, `…/api/impl.rs`): enrutar Agent Mode al `DirectChatClient`. Devolver el mismo tipo de *stream* (`AIOutputStream<…>`) para no tocar el resto de la app.
- Eliminar/inhabilitar la ruta protobuf `multi-agent` y endpoints `/ai/*` hacia Warp.

**B5. UI.** `app/src/settings_view/ai_page.rs`: alta/edición de `DirectProvider` (nombre, `base_url`, `api_key`, headers), botón **"Probar conexión"** (hace el `GET /v1/models`), y aviso de capacidades disponibles (§7).

### PARTE C — Neutralizar auth/sync/update + modo local (decisión #4)

**C1. Modo local/offline.** Permitir arrancar sin login de Warp: *bypass* del *gating* de auth en `onboarding`/startup para que la app sea usable solo con la API configurada.
**C2. Desconectar egress de auth/sync/update/Warp Drive/session-sharing** (quedan cubiertos por el allowlist A1, pero además se desactivan en origen para no romper UX con timeouts/errores).
**C3. Seams para el futuro:** dejar *traits*/puntos de extensión documentados para reimplementar auth/sync/update contra servicios propios más adelante. No implementarlos ahora.

## 6. Fases / milestones

| Fase | Entrega | Depende de |
|------|---------|------------|
| 0 | Branch + `PLAN.md`/`AGENT.md` (este commit) | — |
| 1 | **A1+A2+A3**: allowlist de egress + telemetría/crash off + test de no-egress | — |
| 2 | **B1+B2**: `DirectProvider` + `/v1/models` + feature flag `DirectInferenceProviders` | F1 |
| 3 | **B3**: cliente OpenAI `chat/completions` *streaming* + tool-loop | F2 |
| 4 | **B4+B5**: reemplazo del dispatch (sin `app.warp.dev`) + UI + "Probar conexión" | F3 |
| 5 | **C1+C2**: modo local + desconexión de auth/sync/update en origen | F1 |
| 6 | **C3** + Anthropic `messages` / OpenAI `responses` + endurecimiento | F4/F5 |

## 7. Capacidades tras el cambio (honesto)

- ✅ Chat de Agent Mode (un agente), *streaming*, *tool calling* (cliente), modelos vía `/v1/models`.
- ⚠️ Degradado/desactivado: orquestación multi-agente, ambient agents, sugerencias pasivas, títulos/code-review, transcripción de voz — dependían del backend.
- ❌ Quitado ahora: login Warp, Warp Drive, sync en nube, session sharing, autoupdate (vuelven como *custom* más adelante, decisión #4).
- La UI comunica estas limitaciones al activar un `DirectProvider`.

## 8. Estrategia de pruebas

- **Unit (skill `rust-unit-tests`):** allowlist (acepta API URL/loopback, rechaza el resto); `should_disable_telemetry()`==true; serialización `chat/completions`; parser SSE (fixtures con *tool calls* y errores parciales); `/v1/models` parsing; no-fuga de `api_key`.
- **Integration (skill `warp-integration-test`):** mock server SSE de `chat/completions` + `/v1/models`; Agent Mode extremo a extremo; arranque en modo local sin login.
- **Manual (skill `verify`/`run`):** build OSS arranca sin login, sin egress salvo la API URL (verificar red), y un provider local (Ollama/LM Studio) responde.
- **UI:** aplicar `warp-ui-guidelines` antes de tocar `ai_page.rs`.

## 9. Riesgos y mitigaciones

| Riesgo | Mitigación |
|--------|------------|
| **Quitar auth rompe el arranque/onboarding** (riesgo principal) | Modo local C1 que *bypassa* el *gating*; iterar con la skill `verify` hasta que arranque |
| Muchas features asumen el backend de Warp | Degradar con elegancia (§7); avisos en UI; detrás de flag |
| Adaptar el SSE nativo al modelo de eventos interno (proto) | `stream_adapter` aislado + fixtures; empezar por texto, luego tools |
| Fuga de `api_key` | `secure_storage` + redacción; nunca TOML/`tracing` |
| Allowlist demasiado estricto bloquea algo necesario (p. ej. OAuth de MCP) | Allowlist configurable; loopback siempre permitido; documentar excepciones explícitas |
| *Merge conflicts* con upstream | Cambios concentrados (choke point + dispatch); flag para aislar |

## 10. Archivos involucrados (rutas verificadas)

**Egress / telemetría (A, C2):** `crates/http_client/src/lib.rs` (allowlist) · `crates/warp_core/src/channel/config.rs` · `app/src/bin/oss.rs` (referencia) · `app/src/settings/privacy.rs` · `app/src/server/telemetry/{mod,collector,macros}.rs` · `app/src/crash_reporting/mod.rs` · `crates/warp_features/src/lib.rs` (`AgentModeAnalytics`).

**IA directa (B):** **NUEVO** `crates/ai/src/providers.rs` · **NUEVO** `crates/ai/src/direct_provider/{mod,openai,stream_adapter}.rs` · `crates/ai/src/api_keys.rs` · `app/src/ai/llms.rs` · `app/src/ai/agent/api.rs` + `…/api/impl.rs` · `app/src/server/server_api.rs` · `app/src/settings_view/ai_page.rs` · `crates/warp_features/src/lib.rs` (flag `DirectInferenceProviders`).

**Auth/local (C):** `crates/onboarding/` · `app/src/auth/` · `app/src/lib.rs` (startup/gating) · `crates/warp_core/src/channel/{config,state}.rs` (rtc/session-sharing/oz).
