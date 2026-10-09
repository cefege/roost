# Agent host contract

The coordinator calls the agent host over HTTP at `ROOST_AGENT_HOST_URL` (the fleet default is `http://127.0.0.1:4115`). Every request except `GET /healthz` carries `Authorization: Bearer <ROOST_AGENT_HOST_SECRET>`. A missing or incorrect bearer is rejected with HTTP 401. Bodies and responses are UTF-8 JSON with snake_case keys. Errors use a 4xx or 5xx status and `{ "error": { "code": "not_found"|"invalid"|"busy"|"unavailable"|"internal", "message": string } }`. The coordinator maps these codes to `NotFound`, `InvalidArgument`, `FailedPrecondition`, `Unavailable`, and `Internal`, respectively. Connection failures and timeouts become `Unavailable` with message `agent host unreachable`.

## Shared JSON values

`ConversationSummary` is `{id, title, worker_fp, worker_label, cwd, model, thinking_level, run_state, error, created_ms, updated_ms}`. `model` is either `{provider, model_id}` or null; `thinking_level` and `error` are nullable. `run_state` is `idle`, `running`, or `failed`.

`Transcript` is `{items, run_state, error, model, thinking_level, usage}`. `usage` is `{input_tokens, output_tokens, cost_usd}`. Items are tagged by `kind`:

- `{id, kind:"user", text}`
- `{id, kind:"assistant", blocks, streaming, error}`
- `{id, kind:"tool", call_id, tool_name, args_json, output, is_error, running}`

Blocks are tagged by `type`: `{type:"text", text}`, `{type:"thinking", text}`, or `{type:"tool_call", call_id, tool_name, args_json}`.

Chat events are tagged by `type`:

- `reset{transcript}` replaces the transcript.
- `item{item}` appends an item or replaces the item with the same id.
- `text_delta{item_id, block, delta}` and `thinking_delta{item_id, block, delta}` append to `blocks[block]`, creating an empty block of the matching type when `block == blocks.len()`.
- `block_set{item_id, block, value}` replaces a block.
- `tool_output{item_id, trim_start, append, set}` replaces output when `set` is present; otherwise it removes `trim_start` leading characters and appends `append`.
- `run_state{run_state, error}`, `agent{model, thinking_level}`, and `usage{usage}` update transcript metadata.

## HTTP endpoints

`GET /v1/events` returns a persistent NDJSON stream. Its first line is `{"type":"hello","protocol":1}`, its second is `{"type":"conversations","conversations":[ConversationSummary]}`, followed by one `chat` line containing a `reset` for every conversation. Later lines are `{"type":"conversation","conversation":ConversationSummary}`, `{"type":"conversation_removed","id":string}`, `{"type":"chat","conversation_id":string,"events":[ChatEvent]}`, or `{"type":"ping"}`. A ping is sent every 15 seconds.

`POST /v1/conversations` takes `{worker_fp, worker_label, worker_os, cwd, model}` where model is `{provider, model_id}` or null, and returns a `ConversationSummary`. A null model selects the `default_model` setting, then the first available model; if no model is signed in the host returns `invalid` with `no model is signed in`.

`POST /v1/conversations/{id}/submit` takes `{text, request_id}` and returns `{}`. The host submits input with `whenBusy:"steer"`. `POST /v1/conversations/{id}/abort` returns `{}`. `POST /v1/conversations/{id}/configure` takes any subset of `{model, thinking_level, worker_fp, worker_label, worker_os, cwd, title}`; omitted fields remain unchanged, and returns the updated summary. `DELETE /v1/conversations/{id}` returns `{}`.

`GET /v1/models` returns `{models, providers, thinking_levels, default_model}`. Each model is `{provider, model_id, name, reasoning, available}`. Each provider is `{id, name, configured, credential, supports_oauth}`; credential is `oauth`, `api_key`, `env`, or null. `default_model` is a model reference or null.

`POST /v1/auth/logins` takes `{provider}` and returns `{login_id}`. `GET /v1/auth/logins/{id}` returns `{state, prompt, notices, error}`. State is `waiting`, `prompt`, `done`, or `failed`; prompt is null or `{id, type, message, options}`, where type is `text`, `secret`, `select`, or `manual_code`. Notices are `{type, message, url, code}`, where type is `info`, `auth_url`, `device_code`, or `progress`. `POST /v1/auth/logins/{id}/respond` takes `{prompt_id, value}` and returns `{}`; `DELETE /v1/auth/logins/{id}` cancels and returns `{}`. `PUT /v1/auth/api-keys/{provider}` takes `{api_key}` and returns `{}`. `DELETE /v1/auth/credentials/{provider}` removes credentials and returns `{}`. `GET /healthz` requires no authentication and returns HTTP 200 with `ok`.

## Worker tool tunnel

The coordinator exposes `GET /internal/agent-env/{worker_fp}` as an internal WebSocket. Admission requires both a loopback TCP peer and `Authorization: Bearer <ROOST_AGENT_HOST_SECRET>` (the presented and configured secrets are compared by their SHA-256 digests). An unconfigured domain returns 404, a non-loopback peer 403, and a bad secret 401. The SPA does not own `/internal/` routes.

The host first sends text `{"type":"open","args":["serve","--token",<32 lowercase hex>],"daemons":{"<platform>-<arch>":"<sha256 hex>",...}}`. Any other argument shape closes with 1008, `invalid args`. It may then upload a daemon with text `{"type":"daemon_chunk_begin","size":N}`, binary daemon bytes, and text `{"type":"daemon_end"}`. Thereafter binary messages are daemon stdin. Coordinator-to-host text messages are `{"type":"need_daemon","platform":"linux-x64"}`, `{"type":"opened"}`, and `{"type":"stderr","text":string}`; binary messages carry daemon stdout. Successful exit closes with code 1000 and reason `exit <code>`. Failures close with 1011 and an explanatory reason, including unroutable worker, missing capability, unsupported platform, SHA mismatch, backlog overflow, or lost worker link.

The coordinator relays the daemon's stdin/stdout bytes opaquely through worker-link frames; it does not parse pi-env framing. The worker-link contract consists of `DAgentTunnelOpen` (tunnel id, args, daemon SHA map), `DAgentTunnelInput` (tunnel id, bytes), `DAgentTunnelDaemonChunk` (tunnel id, bytes, last flag), `DAgentTunnelClose` (tunnel id), `WAgentTunnelState` (tunnel id, state, platform, exit code, error), and `WAgentTunnelOutput` (tunnel id, stderr flag, bytes). The state values are `OPENED`, `NEED_DAEMON`, and `CLOSED`; corresponding serde kinds are `agent-tunnel-open`, `agent-tunnel-input`, `agent-tunnel-daemon-chunk`, `agent-tunnel-close`, `agent-tunnel-state`, and `agent-tunnel-output`. Workers advertise capability `agent_tool_tunnel_v1`.
