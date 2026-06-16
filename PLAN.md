# PLAN.md — Warp sin telemetría + proveedores de IA personalizados (BaseURL)

> Estado: **propuesta de implementación** (no implementado todavía).
> Rama de trabajo: `claude/bold-galileo-08lwrw`.
> Documento complementario: ver `AGENT.md` para las reglas de ejecución, convenciones del repo y *definition of done*.

## 1. Objetivo

Modificar Warp para:

1. **Quitar / desactivar la telemetría** (analytics RudderStack) y el *crash reporting* (Sentry), de forma que el cliente no haga *phone-home* de uso ni de crashes.
2. **Permitir usar una API de IA propia indicando una `BaseURL`**, hablando directamente el protocolo nativo de:
   - **OpenAI** (`POST {base_url}/v1/chat/completions` y/o la Responses API `POST {base_url}/v1/responses`), y
   - **Anthropic** (`POST {base_url}/v1/messages`),
   - además de **proveedores *custom*** compatibles con cualquiera de esos dos formatos de *wire* (p. ej. Ollama, LM Studio, vLLM, OpenRouter, Together, Groq, Azure OpenAI, etc.).

> ⚠️ **Nota de terminología.** El pedido menciona «la `/v1` de OpenAI o `/responses` de Anthropic». En realidad `/responses` es la *Responses API* de **OpenAI**; el endpoint nativo de **Anthropic** es `/v1/messages`. Este plan soporta los tres formatos (`OpenAiChatCompletions`, `OpenAiResponses`, `AnthropicMessages`) y deja que el usuario elija el protocolo por proveedor, respetando la intención original.

## 2. Hallazgos clave de la arquitectura actual (verificados en el código)

Estas dos realidades condicionan todo el diseño:

### 2.1 La telemetría está *gated* por configuración de canal

- `crates/warp_core/src/channel/config.rs` → `ChannelConfig.telemetry_config: Option<TelemetryConfig>` y `crash_reporting_config: Option<CrashReportingConfig>`. El comentario del propio código dice: *"or None if telemetry should be disabled for this build"*.
- `crates/warp_core/src/channel/state.rs` → `ChannelState::is_telemetry_available()` devuelve `false` cuando `telemetry_config` es `None` (y oculta la UI). Igual para `is_crash_reporting_available()`.
- Los binarios `app/src/bin/oss.rs` y `app/src/bin/integration.rs` **ya** se compilan con `telemetry_config: None` y `crash_reporting_config: None`. → **Es el camino más limpio y de menor riesgo.**
- Gate de *runtime*: `app/src/settings/privacy.rs` → `PrivacySettingsSnapshot::should_disable_telemetry()`:
  ```rust
  !self.is_telemetry_enabled
      && !self.is_telemetry_force_enabled            // puede forzar telemetría ON
      && !FeatureFlag::AgentModeAnalytics.is_enabled() // experimento que fuerza ON
  ```
  → El opt-out del usuario **no es absoluto**: `is_telemetry_force_enabled` (override de organización) y el flag `AgentModeAnalytics` pueden re-activarla. Una eliminación seria debe neutralizar estas dos rutas.
- El envío real ocurre en `app/src/server/telemetry/` (`collector.rs`, `mod.rs`, `macros.rs`) hacia RudderStack (`/v1/batch`, `/v1/track`, …) y el *crash reporting* en `app/src/crash_reporting/mod.rs` (Sentry).

### 2.2 TODO el tráfico de IA pasa por el backend de Warp (no por el proveedor)

- `app/src/server/server_api.rs` → `generate_multi_agent_output()` arma la URL:
  ```rust
  format!("{}/{}/{}", ChannelState::server_root_url(), "ai", "multi-agent")
  ```
  y hace `POST {server_root_url}/ai/multi-agent` con un cuerpo **protobuf** `warp_multi_agent_api::Request`, respuesta por **SSE** con eventos `ResponseEvent` codificados en Base64. `server_root_url` por defecto es `https://app.warp.dev` (`crates/warp_core/src/channel/config.rs`).
- **El cliente nunca llama a `api.openai.com` ni a `api.anthropic.com`.** Es el backend de Warp el que hace de proxy hacia los proveedores.
- Las features existentes **no resuelven el pedido por sí solas**:
  - *BYOK* (`ApiKeyManager::api_keys_for_request`) y *Custom Endpoints* (`ApiKeyManager::custom_model_providers_for_request`, en `crates/ai/src/api_keys.rs`) **inyectan la `base_url` + `api_key` dentro del protobuf** que se manda al backend de Warp. Es decir: la clave del usuario y la URL custom **igual viajan a los servidores de Warp**, que son quienes llaman al proveedor. Esto **no** es "hablar directo con OpenAI/Anthropic".
  - Ya existe `ChannelState::override_server_root_url()` + variable de entorno `WARP_SERVER_ROOT_URL` / flag `--server-root-url` (`crates/warp_cli/src/lib.rs`, aplicado en `app/src/lib.rs`). Esto solo permite **re-apuntar el backend de Warp** a otra URL — útil únicamente si esa URL implementa el protocolo protobuf `multi-agent` de Warp.

**Conclusión de diseño:** para cumplir "usar mi API indicando BaseURL y hablar `/v1` de OpenAI o `/v1/messages` de Anthropic" hay que **añadir una ruta de cliente nueva (direct-provider) que evite `generate_multi_agent_output` y hable el protocolo HTTP nativo de cada proveedor**, adaptando su *streaming* al *stream* interno que consume la UI. El plan se centra en esto.

## 3. Alcance y límites (honestos)

El backend de Warp hace mucho trabajo del lado del servidor (orquestación multi-agente, *planning*, *passive suggestions*, *code review*, *block titles*, transcripción de voz, *input prediction*, etc.). La ruta *direct-provider* del cliente, de forma realista:

- ✅ **Soporta**: el bucle principal de chat de Agent Mode (un agente), *streaming* de texto, y *tool calling* (function calling) implementado en el cliente.
- ⚠️ **Degradado / opcional en fases posteriores**: orquestación multi-agente, ambient agents, sugerencias pasivas, generación de títulos/*code review*. Cuando el modelo activo sea un *direct provider*, estas funciones se desactivan o caen a un *fallback* local simple.
- ❌ **Fuera de alcance inicial**: re-implementar 1:1 toda la semántica del protobuf de Warp contra un endpoint crudo.

Esto se comunica en la UI: al elegir un *direct provider*, se indica qué capacidades quedan disponibles.

## 4. Plan de trabajo

### PARTE A — Quitar la telemetría

**A1. Kill-switch a nivel de build (obligatorio, bajo riesgo).**
- Tomar `app/src/bin/oss.rs` como build de referencia (ya tiene `telemetry_config: None` y `crash_reporting_config: None`).
- Para los binarios que se quieran usar (`stable`/`preview`/`dev`/`local`), poner `telemetry_config: None` y `crash_reporting_config: None`. Esto desactiva el envío y oculta la UI mediante `is_telemetry_available()` / `is_crash_reporting_available()`.
- Verificar que `autoupdate_config` y cualquier otro *phone-home* (p. ej. `oz_config`) se evalúe por separado (ver A4).

**A2. Hacer absoluto el opt-out en runtime (defensa en profundidad).**
- En `app/src/settings/privacy.rs`:
  - `should_disable_telemetry()` → devolver `true` incondicionalmente (o ignorar `is_telemetry_force_enabled` y `AgentModeAnalytics`).
  - Cambiar los *defaults*: `is_telemetry_enabled: false`, `is_crash_reporting_enabled: false`.
  - Neutralizar la ruta de *force-enable* desde el servidor (`SyncedUserSettings` / `set_is_telemetry_enabled`) para que no pueda re-activar.
- Neutralizar `FeatureFlag::AgentModeAnalytics` (`crates/warp_features/src/lib.rs`) como vector de re-activación.

**A3. Convertir el envío en no-op (opcional, eliminación profunda).**
- En `app/src/server/telemetry/macros.rs` / `collector.rs`: hacer que `send_telemetry_*` / `record_telemetry_*` y `schedule_event_queue_flush()` no hagan nada (early-return). Mantener las firmas para no romper las ~35 ubicaciones que las invocan.
- Alternativa más quirúrgica: dejar A1+A2 y no tocar los *call sites* (menos diff, menos riesgo de *merge conflicts* con upstream).
- `app/src/crash_reporting/mod.rs`: no inicializar Sentry (ya cubierto si `crash_reporting_config: None`).

**A4. Auditar egress restante (documentar, no necesariamente eliminar).**
- Autoupdate (`autoupdate_config`), Oz/ambient (`oz_config`), `firebase_auth_api_key`, sincronización de settings a la nube. Decidir caso por caso; al menos documentarlo en `AGENT.md`. No confundir telemetría con autenticación/funcionalidad core.

**Decisión recomendada:** A1 + A2 como entrega mínima sólida; A3 como *follow-up* opcional. *(Ver §8 — pregunta abierta sobre la profundidad deseada.)*

### PARTE B — Proveedores de IA personalizados (BaseURL + OpenAI/Anthropic)

**B1. Modelo de datos de configuración.**
- En `crates/ai/src/api_keys.rs` (o un módulo nuevo `crates/ai/src/providers.rs`), añadir un tipo nuevo —distinto del `CustomEndpoint` actual para no confundir la ruta que va por el backend de Warp—:
  ```rust
  pub struct DirectProvider {
      pub name: String,
      pub base_url: String,          // p.ej. https://api.openai.com  | http://localhost:11434
      pub api_key: String,           // se guarda en secure_storage, NUNCA en TOML/logs
      pub protocol: ApiProtocol,     // ver abajo
      pub models: Vec<DirectProviderModel>,
      pub default_headers: Vec<(String, String)>, // p.ej. anthropic-version, azure api-version
  }
  pub enum ApiProtocol {
      OpenAiChatCompletions, // POST {base_url}/v1/chat/completions
      OpenAiResponses,       // POST {base_url}/v1/responses
      AnthropicMessages,     // POST {base_url}/v1/messages
  }
  ```
- Persistencia: la `api_key` va a `secure_storage` (igual que `SECURE_STORAGE_KEY = "AiApiKeys"`); el resto a *user preferences*.

**B2. Cliente directo nuevo (núcleo del feature).**
- Crear módulo `crates/ai/src/direct_provider/` con:
  - `mod.rs` — *trait* `DirectChatClient` + *dispatch* por `ApiProtocol`.
  - `openai.rs` — construye el JSON de `chat/completions` y de `responses`; parsea SSE (`data: {...}`, *deltas*, *tool_calls*, `[DONE]`).
  - `anthropic.rs` — construye el JSON de `messages` (headers `x-api-key`, `anthropic-version`); parsea SSE (`message_start`, `content_block_delta`, `message_delta`, `message_stop`, *tool_use*).
  - `stream_adapter.rs` — mapea el *streaming* nativo a los eventos internos que la UI ya consume (texto incremental, *tool calls*, fin de turno, errores).
- Reutilizar `crates/http_client` (`Client::post`, `.eventsource()`, `.bearer_auth()` / headers custom). No introducir un cliente HTTP nuevo.
- Implementar el **bucle de *tool calling* en el cliente** (lo que normalmente hace el servidor): enviar herramientas disponibles, recibir *tool calls*, ejecutarlas con el *runtime* de tools existente, y reenviar resultados hasta el turno final.

**B3. Punto de *dispatch* (bifurcación cliente vs backend).**
- En la capa que hoy llama a `ServerApi::generate_multi_agent_output()` (ver `app/src/ai/agent/api.rs` / `app/src/ai/agent/api/impl.rs`, donde `RequestParams` arma `settings`): si el modelo seleccionado pertenece a un `DirectProvider`, **enrutar al `DirectChatClient`** en lugar de al backend de Warp. Devolver el mismo tipo de *stream* (`AIOutputStream<…>`) para que el resto de la app no cambie.
- Extender `app/src/ai/llms.rs`: añadir variante de host (p. ej. `LLMModelHost::DirectProvider`) y/o registrar los modelos del `DirectProvider` en el *picker* de modelos (`MODELS_BY_FEATURE_CACHE_KEY`).

**B4. UI de configuración.**
- Extender `app/src/settings_view/ai_page.rs`: reusar el patrón de `CustomEndpointModal` para un alta/edición de `DirectProvider` con campos: nombre, `base_url`, protocolo (combo), `api_key`, lista de modelos, headers opcionales, y botón **"Probar conexión"**.
- Mostrar claramente las **limitaciones** (ver §3) cuando el usuario activa un *direct provider*.

**B5. Feature flag.**
- Añadir un flag nuevo (p. ej. `DirectInferenceProviders`) en `crates/warp_features/src/lib.rs` usando la skill `add-feature-flag`. Todo lo de la Parte B queda detrás del flag hasta estabilizar. No reutilizar `CustomInferenceEndpoints` (semántica distinta: esa ruta va por el backend).

## 5. Fases / milestones

| Fase | Entrega | Depende de |
|------|---------|------------|
| 0 | Branch + `PLAN.md`/`AGENT.md` (este commit) | — |
| 1 | **Parte A1+A2**: build sin telemetría + opt-out absoluto + tests | — |
| 2 | **B1+B5**: modelo de datos `DirectProvider` + feature flag + persistencia segura | Fase 0 |
| 3 | **B2 (OpenAI)**: `chat/completions` *streaming* + *tool loop*, detrás del flag | Fase 2 |
| 4 | **B2 (Anthropic `messages`) + OpenAI `responses`** | Fase 3 |
| 5 | **B3+B4**: *dispatch* real + UI + "Probar conexión" | Fase 3/4 |
| 6 | A3 (no-op profundo) + endurecimiento + docs | Fase 1 |

## 6. Estrategia de pruebas

- **Unit tests (Rust)** con la skill `rust-unit-tests`:
  - Parte A: `should_disable_telemetry()` siempre `true`; *defaults* en `false`; el *force-enable* no re-activa.
  - Parte B: *serialización* de request por protocolo; *parser* de SSE (fixtures OpenAI/Anthropic, incluido *tool calling* y errores parciales); que la `api_key` nunca aparezca en logs/serialización a TOML.
- **Integration tests** con la skill `warp-integration-test`: un *mock server* HTTP que emule `chat/completions` y `messages` por SSE, y verificar el *stream* extremo a extremo en Agent Mode.
- **Manual / verify** con la skill `verify` o `run`: levantar el binario OSS y confirmar (a) que no hay tráfico a RudderStack/Sentry (revisar red), y (b) que un proveedor local (p. ej. Ollama / LM Studio) responde en Agent Mode.
- **Guía UI**: aplicar `warp-ui-guidelines` antes de escribir la `ai_page.rs`.

## 7. Riesgos y mitigaciones

| Riesgo | Mitigación |
|--------|------------|
| *Merge conflicts* con upstream al tocar los ~35 *call sites* de telemetría | Preferir A1+A2 (cambios localizados); dejar A3 como opcional |
| Romper Agent Mode al bifurcar el *dispatch* | Todo detrás de `DirectInferenceProviders`; *fallback* al backend si el flag está off o el provider falla |
| Paridad de features incompleta (multi-agente, etc.) | Comunicar límites en UI (§3); degradar con elegancia |
| Fuga de `api_key` del usuario | `secure_storage` + redacción; nunca a TOML ni a `tracing` |
| Diferencias de *wire format* entre proveedores "compatibles" | `ApiProtocol` + headers configurables + botón "Probar conexión" |
| Confundir telemetría con auth/funcionalidad (romper login) | A4 audita egress sin tocar auth core |

## 8. Preguntas abiertas (para confirmar con el usuario)

1. **Profundidad de la eliminación de telemetría**: ¿basta con A1+A2 (recomendado, build OSS sin envío + opt-out absoluto) o se quiere también A3 (convertir los *call sites* en no-op / borrar el módulo)?
2. **Egress no-telemetría** (autoupdate, sincronización de settings, Firebase auth, Oz): ¿se desactivan también o solo la telemetría/analytics?
3. **¿Reemplazar el backend de Warp o convivir?** ¿El *direct provider* debe ser una opción adicional (recomendado) o el único camino (quitando del todo la ruta `app.warp.dev`)?
4. **Protocolos prioritarios**: ¿empezamos por OpenAI `chat/completions` (mayor compatibilidad: Ollama, vLLM, OpenRouter…) y luego Anthropic `messages`, o al revés?

## 9. Archivos involucrados (mapa rápido, rutas verificadas)

**Telemetría (Parte A)**
- `crates/warp_core/src/channel/config.rs` — `telemetry_config` / `crash_reporting_config` (`Option`).
- `crates/warp_core/src/channel/state.rs` — `is_telemetry_available()`, gates.
- `app/src/bin/oss.rs` *(referencia, ya None)*, `app/src/bin/{stable,preview,dev,local}.rs` *(targets)*.
- `app/src/settings/privacy.rs` — `should_disable_telemetry()`, *defaults*, force-enable.
- `app/src/server/telemetry/{mod,collector,macros}.rs` — envío a RudderStack.
- `app/src/crash_reporting/mod.rs` — Sentry.
- `crates/warp_features/src/lib.rs` — `AgentModeAnalytics`.

**Proveedores custom (Parte B)**
- `crates/ai/src/api_keys.rs` — `ApiKeys`, `CustomEndpoint` (referencia), nuevo `DirectProvider`.
- **NUEVO** `crates/ai/src/direct_provider/{mod,openai,anthropic,stream_adapter}.rs`.
- `app/src/ai/llms.rs` — `LLMProvider`, `LLMModelHost` (+ `DirectProvider`).
- `app/src/ai/agent/api.rs`, `app/src/ai/agent/api/impl.rs` — armado de request / punto de dispatch.
- `app/src/server/server_api.rs` — `generate_multi_agent_output()` (ruta backend existente).
- `crates/http_client/src/lib.rs` — cliente HTTP/SSE a reutilizar.
- `app/src/settings_view/ai_page.rs` — UI.
- `crates/warp_features/src/lib.rs` — nuevo flag `DirectInferenceProviders`.
