# AGENT.md — Guía de ejecución para el trabajo de `PLAN.md`

> Para el agente/persona que **implemente** `PLAN.md`: Warp **local-first** (sin conexiones externas salvo la API de IA configurada) con **cliente directo a OpenAI `chat/completions`**.
> El **qué/por qué** está en `PLAN.md`; esto es el **cómo** en este repo. Léela completa antes de tocar código.

## 1. Misión en una frase

Que Warp **solo** salga a la red hacia la **API de IA que el usuario configure** (todo lo demás bloqueado, **sin opt-in**), hable **directo** con esa API (OpenAI `chat/completions`, modelos vía `GET /v1/models`) **sin pasar por `app.warp.dev`**, y arranque en **modo local** sin login. Auth/sync/update se neutralizan ahora; vuelven como *custom* después.

## 2. Decisiones ya cerradas (no re-preguntar)

1. Empezar por **OpenAI `chat/completions`**; modelos desde `GET /v1/models`.
2. **Sin conexión externa** salvo la API URL configurada; **no opt-in**.
3. **Quitar el hop `app.warp.dev`** para IA → directo a la API.
4. **Auth/sync/update**: neutralizar ahora; **modo local** para arrancar sin login; soluciones custom más adelante.

## 3. Orientación del repositorio

- **Choke point de red:** `crates/http_client/src/lib.rs` → aquí va el **allowlist de egress**.
- **Telemetría/privacidad:** `crates/warp_core/src/channel/config.rs` (`telemetry_config`/`crash_reporting_config` `Option`), `app/src/settings/privacy.rs` (`should_disable_telemetry`), `app/src/server/telemetry/`, `app/src/crash_reporting/mod.rs`, `app/src/bin/oss.rs` (referencia ya sin telemetría).
- **IA:** `app/src/server/server_api.rs` (`generate_multi_agent_output` = ruta a reemplazar), `crates/ai/src/api_keys.rs`, `app/src/ai/llms.rs`, `app/src/ai/agent/api.rs` + `…/api/impl.rs`, **NUEVO** `crates/ai/src/{providers.rs,direct_provider/}`.
- **UI:** `app/src/settings_view/ai_page.rs`.
- **Auth/startup:** `crates/onboarding/`, `app/src/auth/`, `app/src/lib.rs`.
- **Flags:** `crates/warp_features/src/lib.rs`.

## 4. Invariantes y *gotchas* (NO los rompas)

1. **El egress se controla en `http_client` (A1), no por feature.** Permitir solo host(s) de la `base_url` + loopback; bloquear el resto. Es la garantía dura de la decisión #2.
2. **El cliente hoy NO habla con OpenAI directo** (todo va por `app.warp.dev` en protobuf). Hay que **reemplazar** esa ruta con `crates/ai/src/direct_provider/`, no parchear *BYOK*/`CustomEndpoint` (esos siguen pasando por Warp).
3. **Opt-out de telemetría no es absoluto:** neutraliza `is_telemetry_force_enabled` y `FeatureFlag::AgentModeAnalytics` además del toggle.
4. **Quitar auth puede romper el arranque** → implementa **modo local** (C1) y valida con `verify` que la app abre sin login.
5. **API keys = secretos:** `secure_storage`, nunca TOML/`tracing`/errores. Redacta.
6. **Reutiliza `crates/http_client`** (no agregues otro cliente HTTP).
7. **Parte B detrás de `DirectInferenceProviders`**; con flag off, IA queda inerte (no vuelve a `app.warp.dev`).
8. Trata `base_url`, headers y respuestas del modelo como **entrada no confiable**: valida URL, no sigas redirecciones fuera del allowlist, ejecuta tools solo por el *runtime* existente.

## 5. Skills a usar

- `add-feature-flag` → `DirectInferenceProviders`.
- `rust-unit-tests` → unit tests (§7 de `PLAN.md`).
- `warp-integration-test` → mock SSE de `chat/completions` + `/v1/models`, arranque sin login.
- `warp-ui-guidelines` → **antes** de tocar `ai_page.rs`.
- `verify` / `run` → arrancar build OSS: sin login, sin egress salvo API URL, provider local responde.
- `add-telemetry` → solo **referencia** del sistema de eventos (para desmontarlo), no para añadir.
- `claude-api` → referencia al implementar Anthropic en fase posterior.

## 6. Flujo de trabajo y git

- Rama **siempre** `claude/bold-galileo-08lwrw` (créala si no existe). No push a otra rama sin permiso.
- Commits pequeños, uno por hito (§6 de `PLAN.md`), en imperativo.
- `git push -u origin claude/bold-galileo-08lwrw`; si falla por red, backoff 2s/4s/8s/16s (máx. 4).
- **No abrir PR** salvo pedido explícito. No incluir identificadores internos de modelo/agente en artefactos del repo.

## 7. Calidad y verificación (antes de cada push)

- `cargo build` del binario OSS; `cargo fmt` + `cargo clippy` (respeta `.rustfmt.toml`/`.clippy.toml`).
- Tests unit + integración (§8 de `PLAN.md`) pasan.
- Verificación manual: build OSS arranca **sin login**, **sin egress** salvo la API URL (revisar red), provider local (Ollama/LM Studio) responde en Agent Mode.
- Reporta con honestidad: si algo falla, muéstralo con su salida; si saltaste un paso, dilo.

## 8. Definition of Done

**A — Lockdown:** [ ] allowlist en `http_client` (acepta API URL/loopback, rechaza el resto) · [ ] `telemetry_config`/`crash_reporting_config` `None` + `should_disable_telemetry()`==true incondicional · [ ] test que falla si hay egress a otro host.

**B — IA directa:** [ ] `DirectProvider`+`ApiProtocol::OpenAiChatCompletions`, key en `secure_storage` · [ ] `GET /v1/models` puebla el picker (+fallback) · [ ] cliente `chat/completions` *streaming* + tool-loop en cliente · [ ] dispatch reemplazado: Agent Mode NO toca `app.warp.dev` · [ ] UI con alta/edición + "Probar conexión" · [ ] todo tras `DirectInferenceProviders`.

**C — Local:** [ ] la app arranca y es usable **sin login** · [ ] auth/sync/update sin egress · [ ] *seams* documentados para futuras soluciones custom.

## 9. Cuándo parar y preguntar (`AskUserQuestion`)

Las 4 decisiones grandes están cerradas (§2). Pregunta solo si:
- El **modo local** exige cambios de UX no triviales (p. ej. qué mostrar donde iba el login) con varias interpretaciones válidas.
- El allowlist debería permitir algún host extra legítimo (p. ej. OAuth de un MCP server) — confirmar antes de abrir una excepción.
- Aparece un *refactor* grande no previsto o ambigüedad arquitectónica seria.

Para defaults razonables ya cubiertos aquí: procede y menciónalo.
