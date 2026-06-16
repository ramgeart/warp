# AGENT.md — Guía de ejecución para el trabajo de `PLAN.md`

> Esta guía es para el agente (o la persona) que **implemente** lo descrito en `PLAN.md`:
> quitar telemetría + habilitar proveedores de IA por `BaseURL` (OpenAI `/v1`, Anthropic `/v1/messages`, custom).
> Léela completa antes de tocar código. El **qué/por qué** está en `PLAN.md`; esta guía es el **cómo** en este repo.

## 1. Misión en una frase

Hacer que Warp **no envíe telemetría** y pueda **conversar directamente con una API de IA propia** (indicando `BaseURL` + protocolo OpenAI/Anthropic), sin romper Agent Mode ni la autenticación, y dejando todo lo nuevo **detrás de un feature flag**.

## 2. Orientación del repositorio

Monorepo Rust. Rutas que importan para esta tarea:

- **Telemetría / privacidad**
  - `crates/warp_core/src/channel/config.rs` — `ChannelConfig` (`telemetry_config`, `crash_reporting_config` son `Option<…>`).
  - `crates/warp_core/src/channel/state.rs` — `is_telemetry_available()`, `is_crash_reporting_available()`.
  - `app/src/settings/privacy.rs` — `PrivacySettingsSnapshot::should_disable_telemetry()`, *defaults*, force-enable.
  - `app/src/server/telemetry/` — colector RudderStack + macros de envío.
  - `app/src/crash_reporting/mod.rs` — Sentry.
  - `app/src/bin/oss.rs` — **build de referencia ya sin telemetría** (`telemetry_config: None`).
- **IA / red**
  - `app/src/server/server_api.rs` — `generate_multi_agent_output()` = `POST {server_root_url}/ai/multi-agent` (protobuf+SSE).
  - `crates/ai/src/api_keys.rs` — `ApiKeys`, `CustomEndpoint`, `api_keys_for_request`, `custom_model_providers_for_request`.
  - `app/src/ai/llms.rs` — `LLMProvider`, `LLMModelHost`, `LLMInfo`, *picker* de modelos.
  - `app/src/ai/agent/api.rs` + `app/src/ai/agent/api/impl.rs` — `RequestParams`, armado de `settings`.
  - `crates/http_client/src/lib.rs` — wrapper de `reqwest` con `.proto()`, `.eventsource()`, `.bearer_auth()`.
  - `app/src/settings_view/ai_page.rs` — UI de ajustes de IA.
  - `crates/warp_features/src/lib.rs` — *feature flags*.

## 3. Invariantes y *gotchas* (NO los rompas)

1. **El cliente hoy NUNCA habla con OpenAI/Anthropic directamente.** Todo va al backend de Warp (`app.warp.dev/ai/...`) como **protobuf** vía SSE. Las features *BYOK* y *Custom Endpoints* meten la `api_key`/`base_url` **dentro** del protobuf → siguen pasando por servidores de Warp. Para cumplir el objetivo hay que crear una **ruta de cliente nueva** (`crates/ai/src/direct_provider/`) que evite `generate_multi_agent_output`.
2. **No reutilices `CustomEndpoint` para la ruta directa.** Tiene semántica "va por el backend". Crea un tipo nuevo `DirectProvider` para no introducir bugs sutiles de enrutado.
3. **Opt-out de telemetría NO es absoluto por defecto:** `is_telemetry_force_enabled` y `FeatureFlag::AgentModeAnalytics` pueden re-activarla. Si el objetivo es "cero telemetría", hay que neutralizar esas dos rutas además del toggle.
4. **El camino de menor riesgo para telemetría es a nivel build** (`telemetry_config: None`, como `oss.rs`), no editar los ~35 *call sites*.
5. **Las API keys son secretos.** Van a `secure_storage` (ver `SECURE_STORAGE_KEY` en `api_keys.rs`), **nunca** a TOML, ni a `tracing`/logs, ni a mensajes de error. Aplica redacción.
6. **Reutiliza `crates/http_client`** para HTTP/SSE. No agregues otro cliente HTTP.
7. **Todo lo nuevo (Parte B) detrás de `DirectInferenceProviders`.** Con el flag off, comportamiento idéntico al actual.
8. **No confundas telemetría con auth/funcionalidad.** Firebase auth, autoupdate, sync de settings y Oz son rutas distintas; no las desactives sin que sea pedido explícito (ver pregunta abierta §8 de `PLAN.md`).

## 4. Skills disponibles que DEBES usar

Este repo trae *skills* específicas; úsalas en vez de improvisar:

- `add-feature-flag` → crear `DirectInferenceProviders` (y, si hiciera falta, otro para gating).
- `remove-feature-flag` / `promote-feature` → al estabilizar.
- `rust-unit-tests` → escribir y correr los tests unitarios (ver §6).
- `warp-integration-test` → tests de integración con el framework Builder/TestStep.
- `warp-ui-guidelines` → **leerla antes** de tocar `ai_page.rs` u otra UI.
- `add-telemetry` → solo como **referencia** de cómo funciona el sistema de eventos (para desmontarlo con criterio); no para añadir eventos.
- `verify` / `run` → levantar la app y comprobar comportamiento real (sin tráfico a RudderStack/Sentry; provider local respondiendo).
- `claude-api` / `claude-code-guide` → referencia de formatos/idioms de la API de Anthropic al implementar `anthropic.rs`.

## 5. Flujo de trabajo y git

- Trabaja **siempre** en la rama `claude/bold-galileo-08lwrw`. Créala localmente si no existe. No hagas push a otra rama sin permiso explícito.
- Commits pequeños y descriptivos, uno por hito de `PLAN.md` (§5). Mensajes en imperativo.
- `git push -u origin claude/bold-galileo-08lwrw`. Si falla por red, reintenta con backoff exponencial (2s, 4s, 8s, 16s), máx. 4 veces.
- **No abras un Pull Request salvo que el usuario lo pida explícitamente.**
- No incluyas identificadores internos del modelo/agente en commits, comentarios de código ni artefactos del repo.

## 6. Calidad y verificación (antes de cada push)

- Compila: `cargo build` del/los binario(s) afectado(s) (al menos el OSS: ver `app/src/bin/oss.rs`).
- Lint/format: respeta `.rustfmt.toml` y `.clippy.toml` (`cargo fmt`, `cargo clippy`).
- Tests:
  - Unit (skill `rust-unit-tests`): telemetría (`should_disable_telemetry` siempre `true`, *defaults* `false`, force-enable no re-activa) y proveedores (serialización por protocolo, parser SSE con fixtures OpenAI/Anthropic incl. *tool calls* y errores, no-fuga de `api_key`).
  - Integration (skill `warp-integration-test`): mock server SSE emulando `chat/completions` y `messages`, validando Agent Mode extremo a extremo.
- Verificación manual (skill `verify`/`run`): build OSS levanta, **sin** egress a RudderStack/Sentry, y un provider local (Ollama/LM Studio) responde en Agent Mode.
- Reporta resultados con honestidad: si un test falla, dilo con su salida; si saltaste un paso, dilo.

## 7. Definition of Done

**Parte A (telemetría):**
- [ ] El build objetivo se compila con `telemetry_config: None` y `crash_reporting_config: None` (UI de telemetría oculta vía `is_telemetry_available()`).
- [ ] `should_disable_telemetry()` devuelve `true` de forma incondicional; *defaults* de privacidad en `false`; ni force-enable ni `AgentModeAnalytics` re-activan.
- [ ] Verificado en runtime que no sale tráfico a RudderStack ni Sentry.
- [ ] Tests unitarios cubren las rutas anteriores y pasan.

**Parte B (proveedores custom):**
- [ ] Tipo `DirectProvider` + `ApiProtocol { OpenAiChatCompletions, OpenAiResponses, AnthropicMessages }`, con `api_key` en `secure_storage`.
- [ ] `crates/ai/src/direct_provider/` con clientes OpenAI y Anthropic, parser SSE y *tool loop* en cliente.
- [ ] *Dispatch*: con un `DirectProvider` seleccionado, Agent Mode usa el cliente directo y NO toca `app.warp.dev`.
- [ ] UI en `ai_page.rs` para alta/edición + "Probar conexión", con aviso de límites (§3 de `PLAN.md`).
- [ ] Todo detrás de `DirectInferenceProviders`; con flag off, comportamiento idéntico al actual.
- [ ] Tests unit + integración pasan; verificación manual contra un provider local OK.

## 8. Cuándo parar y preguntar

Usa `AskUserQuestion` (no asumas) si:
- Hay que decidir la **profundidad** de la eliminación de telemetría (A1+A2 vs A3) — ver pregunta abierta 1 de `PLAN.md`.
- Hay que tocar **egress no-telemetría** (autoupdate, sync de settings, Firebase, Oz) — pregunta 2.
- El cambio implicaría **quitar del todo** la ruta del backend de Warp (no solo añadir la directa) — pregunta 3.
- Aparece ambigüedad arquitectónica significativa o un *refactor* grande no previsto.

En cambio, **no** preguntes por defaults razonables ya cubiertos aquí: procede y menciónalo.

## 9. Guardarraíles de seguridad

- Trata `base_url`, `default_headers` y respuestas del proveedor como **entrada no confiable**: valida URLs, no sigas redirecciones a destinos arbitrarios sin control, y no ejecutes contenido del modelo fuera del *tool runtime* existente.
- Nunca registres ni serialices `api_key` en claro.
- Esta es una modificación legítima de un proyecto open source para uso propio/privacidad; mantén los cambios trazables y revisables (commits claros, flag, tests).
