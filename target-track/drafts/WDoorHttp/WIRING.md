# WDoorHttp — phase-2 handoff (self-sufficient)

Slice: W-DOOR HTTP half of Stage 2W. Ports v2 `apps/worker/src/local-door/local-ui-server.ts`
(routes, Host/Origin gate, bootstrap, the two WebSocket upgrades, SPA fallback, `server.stop(true)`),
the server half of `apps/worker/src/boot/boot-local-terminal.ts` (`startLocalUiServer(...)`, `server.close()`),
`packages/host/src/http-security.ts` (shared CSP builder) and the worker's use of `packages/host/src/spa.ts`
(`createSpaResponder`, disk build only — v3 embeds no web assets).
Everything below is drafted under `target-track/drafts/WDoorHttp/` mirroring repo paths. Nothing tracked was edited
and no cargo was run in phase 1. Draft files were formatted with standalone `rustfmt --edition 2024` (NOT cargo fmt).

## 0. Order of work for the phase-2 agent
1. Read `target-track/wave-gates/wave2-go` and current `crates/roost-worker/src/runtime/{owners.rs,boot_sequence.rs,mod.rs}`.
2. WDoorTerm (`agent://WorkerLead2W2.WDoorTerm`) must have landed `crate::local_terminal` + `owners.local_terminal`
   (see §4) before §2.6/§2.7 and `tests/local_door_terminal.rs` can compile. Everything else is independent of them.
3. Copy the new files (§1), apply the edits (§2), `/tmp/wcheck.sh 'door|local_ui_door|http_security|coord_config_origin|owners|boot_sequence|local_door'`.
4. Run tests (§3), do mutations (§5), clippy (`/tmp/wcargo.sh clippy -p roost-worker -p roost-host -p roost-protocol --all-targets -- -D warnings`),
   write `wave-gates/done-WDoorHttp`, report.

## 1. New files (copy verbatim from drafts)
| Draft path | Purpose |
|---|---|
| crates/roost-worker/src/door/admission.rs | `DoorAdmission`: Host allowlist (127.0.0.1/localhost/[::1] + bound port), Origin allowlist (those as http:// + coordinator origin + `ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS`), `connect_origins()` = [coordinator origin, ws/wss twin], `cors_origin()`. v2 allowedHosts/allowedOrigins/coordinatorConnectOrigins/browserOrigins. 2 unit tests. |
| crates/roost-worker/src/door/loopback_socket.rs | `LoopbackSocket` (v2 Bun `nativeSocket`): `send(Vec<u8>) -> LoopbackSend {Written,Queued,Dropped}` (Bun ws.send >0/-1/0; Written iff nothing unflushed is queued; Dropped when closed or backlog >16 MiB = Bun default backpressureLimit), `close(u16,&str)` idempotent, `is_open()`; crate-private `open()`, `ping()`, `mark_closed()`, `OutboundQueue::drain_into(sink)` writer. 1 unit test. |
| crates/roost-worker/src/door/loopback.rs | ONE pump for both routes (v2 websocket open/message/close + `guarded`): trait `LoopbackHandlers {type Port; port(); on_open(); on_message(); on_close()}` (Err → log `local_direct_socket_rejected` + close 1011 "local direct handler failed"), `LoopbackRoute {Terminal, Attachment}` (`name()`, `subprotocol()`), type-erased `LoopbackOwner::{terminal,attachment}(Arc<H>)`, `LoopbackRoutes {terminal, attachment: Option}`, `door_stopped()`. Pump: mints nothing (id comes from routes), logs `local_{terminal,attachment}_socket_{opened,closed}`, drops text frames (warn `non_binary_frame`), attachment frame > `LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES` → warn + close 1009 "attachment frame too large", Bun idle rule (120 s silence → ping, another 120 s → end), ends on peer close/error, on writer end (server close sent), or door stop; `on_close` exactly once after the end. |
| crates/roost-worker/src/door/spa.rs | `SpaMount::{from_dist_path, root, respond(path, &Method, accept_encoding)}` — worker-side mirror of `roost-coord/src/http/spa.rs` over `roost_host::spa_path::resolve`; gzip on demand (async-compression) with memo; v2 cache quirk kept: deep-link shell = "no-cache, no-store, must-revalidate", `/index.html` requested by name = "no-cache". |
| crates/roost-worker/src/door/spa_cache.rs | gzip memo keyed (path, mtime, len), 32 entries (v2 `gzipCached`, one entry per path). |
| crates/roost-worker/src/runtime/door_routes.rs | `DoorState` + `router(Arc<DoorState>)` (single `fallback(fetch)`, mirrors v2's one fetch fn): Host 403 `host_not_local` → Origin 403 `origin_not_local` → bootstrap (OPTIONS 204 + CORS preflight headers incl. `access-control-allow-private-network: true`, GET/HEAD 200 JSON `no-store` + ACAO/vary origin, else 405 `bootstrap_method`) → `/ws/local-terminal` upgrade → attachment path (None → 404 `attachment_unavailable`) → non GET/HEAD 405 `method_not_allowed` → SPA. Upgrade order: 405 `{route}_method`, 400 `{route}_subprotocol`, 400 `{route}_not_upgradable`, 500 `{route}_socket_id` (mint failure; `crate::session::ids::mint_uuid` = v2 randomUUID), then `protocols([sub]).max_message_size(1 MiB).max_frame_size(1 MiB).on_upgrade(owner.serve_socket(..))`. Every refusal logs `local_ui_rejected` and has an empty body; every response except 101 gets the security headers. |
| crates/roost-host/src/http_security.rs | (cross-owner, roost-host) `build_csp(relaxed, &[String])`, `security_headers(relaxed, hsts, &[String]) -> Vec<(&'static str, String)>` = v2 `applySecurityHeaders`. 2 unit tests. |
| crates/roost-worker/tests/door_support/mod.rs | real door on 127.0.0.1:0 with recording owners (`DoorEvent::{Opened(Arc<RecordedPort>),Frame,Closed}`), `DoorOptions {coordinator_url, allowed_browser_origins, web_dist, attachment: bool, terminal: Option<LoopbackOwner>, attachment_owner: Option<LoopbackOwner>}`, raw HTTP/1.1 `request()` (custom Host header), `open_socket()` (tokio-tungstenite `client_async`), `next_event()`. Also used by WAttachDirect (wave 3). |
| crates/roost-worker/tests/local_door_http.rs | port of v2 local-ui-server.test.ts HTTP cases (8 tests). |
| crates/roost-worker/tests/local_door_spa.rs | deep link → shell (+gzip), bundle immutable / stale bundle 404, no build → 404 (3 tests). Uses `#[path = "credential_support/scratch.rs"] mod scratch;`. |
| crates/roost-worker/tests/local_door_sockets.rs | v2 socket cases + local-attachment-ui.test.ts (7 tests). |
| crates/roost-worker/tests/local_door_terminal.rs | e2e with WDoorTerm's real `LocalTerminalSockets` via their `tests/local_terminal_support/mod.rs` + `tests/terminal_stream_support/mod.rs` (3 tests: unknown grant → `closed` + 1000; granted → `ready`; no Hello → closed "local terminal hello timed out" + 1000 after `TERMINAL_PEER_HELLO_DEADLINE_MS` (3 s real time)). Adjust imports to whatever WDoorTerm's support actually exports (names used: `Fixture{sockets}`, `GRANT_ID, SECRET, TAB, case, closed_reason, encode, hello`). |

## 2. Edits to existing files
### 2.1 crates/roost-protocol/src/local_ui_door.rs (cross-owner) — replace with draft
Adds `WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV = "ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS"`, `LOCAL_TERMINAL_SUBPROTOCOL = "roost-local-terminal"`,
`LOCAL_TERMINAL_PATH = "/ws/local-terminal"`, `LOCAL_BOOTSTRAP_PATH = "/api/local-bootstrap"`, `LOCAL_TERMINAL_MAX_PAYLOAD_BYTES = 1 MiB`,
`LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES = 4 MiB` (moved out of worker door/mod.rs; still no imports — wasm-safe). Existing items unchanged.
### 2.2 crates/roost-worker/src/door/mod.rs — replace with draft
Declares `pub mod admission; pub mod loopback; mod loopback_socket; pub mod spa; mod spa_cache;`, `pub use loopback_socket::{LoopbackSend, LoopbackSocket};`,
re-exports the 5 protocol constants, keeps `LOCAL_ATTACHMENT_PATH`/`LOCAL_ATTACHMENT_SUBPROTOCOL`, adds `LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES = 1 MiB`.
Header no longer says local_door owns grant digests (WDoorTerm moves them to `local_terminal::grants`).
### 2.3 crates/roost-worker/src/runtime/door_serve.rs — replace with draft
Bind now accepts exactly `127.0.0.1` / `[::1]` (v2 `parseLoopbackBind`; `127.0.0.2` refused). Adds `LocalDoor::serve(self, &DoorConfig, LoopbackRoutes) -> anyhow::Result<DoorServer>`
(spawns `axum::serve(listener, router).with_graceful_shutdown(door_stopped)`, logs `local_ui_listening` / error when no build), `DoorServer {address(), origin(), close()}` (Drop sends stop → listener stops, every pump ends, owners get on_close),
`DoorConfig {coordinator_url, worker_fingerprint, allowed_browser_origins, web_dist}` with `for_boot(&WorkerBoot)` (ProcessEnv) and `from_env(env, url, fp)` (comma split, trim, drop blanks; `ROOST_WEB_DIST_PATH` via `roost_host::ENV_WEB_DIST_PATH`).
Removes the unused `listener()` and `DEFAULT_DOOR_ORIGIN` (grep showed no users) and `port_of`. Tests no longer bind 4114 (were binding the default!). `ENV_DOOR_BIND` kept (used by tests/retire_support/child.rs and boot_sequence).
### 2.4 crates/roost-host/src/lib.rs (cross-owner)
After `pub mod host_memory;` add `pub mod http_security;`. In `pub use coord_config_origin::{...}` add `browser_origin`.
### 2.5 crates/roost-host/src/coord_config_origin.rs (cross-owner)
Replace `const DEFAULT_PORTS: [(&str, u16); 2] = [("http", 80), ("https", 443)];` with
```rust
const DEFAULT_PORTS: [(&str, u16); 5] = [("http", 80), ("https", 443), ("ws", 80), ("wss", 443), ("ftp", 21)];

/// Schemes whose URLs carry a tuple origin; a browser reports every other
/// scheme's origin as opaque (`"null"`).
const TUPLE_ORIGIN_SCHEMES: [&str; 5] = ["http", "https", "ws", "wss", "ftp"];
```
(existing callers only accept http/https, so their answers are unchanged). After `validate_bare_https_origin` add
```rust
/// The origin a browser's `URL` reports for `value` (`new URL(value).origin`),
/// or `None` when `value` does not parse or its origin is opaque.
#[must_use]
pub fn browser_origin(value: &str) -> Option<String> {
    let parsed = parse_origin(value)?;
    TUPLE_ORIGIN_SCHEMES
        .contains(&parsed.scheme.as_str())
        .then(|| parsed.to_origin_string())
}
```
and in `mod tests` (import `browser_origin`) add
```rust
    #[test]
    fn a_browser_origin_is_what_a_url_reports() {
        assert_eq!(browser_origin("HTTPS://Coord.Example:443/path?q#f").as_deref(), Some("https://coord.example"));
        assert_eq!(browser_origin("http://user:pw@coord.test:4102/").as_deref(), Some("http://coord.test:4102"));
        assert_eq!(browser_origin("wss://door:443").as_deref(), Some("wss://door"));
        assert_eq!(browser_origin("file:///tmp/page"), None);
        assert_eq!(browser_origin("not a url"), None);
    }
```
### 2.6 crates/roost-worker/src/runtime/owners.rs (shared; W1Wire owns, WDoorTerm adds `local_terminal: Arc<LocalTerminalDoor>`)
Add to `impl WorkerOwners`:
```rust
    /// The owners the loopback door upgrades sockets into (v2 `startLocalUiServer`'s
    /// `terminal` and `attachment`). The attachment owner is absent until it
    /// lands, and the door answers its path with v2's absent-owner 404.
    pub fn loopback_routes(&self) -> LoopbackRoutes {
        LoopbackRoutes {
            terminal: LoopbackOwner::terminal(self.local_terminal.sockets()),
            attachment: None,
        }
    }
```
with `use crate::door::loopback::{LoopbackOwner, LoopbackRoutes};`. WAttachDirect (wave 3) flips `attachment: None` to `Some(LoopbackOwner::attachment(..))`.
### 2.7 crates/roost-worker/src/runtime/boot_sequence.rs (shared; W1Wire said 14 lines headroom, keep net +1)
Right after the `let owners = super::owners::WorkerOwners::build(...);` statement add:
```rust
    let door = door.serve(&super::door_serve::DoorConfig::for_boot(&boot), owners.loopback_routes())?;
```
At the shutdown tail replace `owners.shutdown();\n    drop(door);` with `door.close();\n    owners.shutdown();` (v2 close(): server.close() first, then disposeDirect/view.dispose). The stopping log's `door = %door.origin()` keeps working (`DoorServer::origin`).
Serving therefore starts once the owners exist (before adoption/readiness) and ends at shutdown — "for the life of link.run" per plan; v2 served even earlier (before sessions), connections made during boot wait in the listen backlog.
### 2.8 crates/roost-worker/src/runtime/mod.rs
Add `pub mod door_routes;` (next to `pub mod door_serve;`).
### 2.9 crates/roost-worker/Cargo.toml (+ Cargo.lock)
`[dependencies]` add `async-compression.workspace = true` (workspace pin has features tokio+gzip; already in Cargo.lock via coord → only the roost-worker dependency list in Cargo.lock gains `"async-compression"`). Tests also use it (GzipDecoder). Dev-deps already have tokio-tungstenite `handshake` (client_async) and futures-util is a normal dep.

## 3. Tests (v2 → Rust)
| v2 test | Rust test |
|---|---|
| local-ui-server.test.ts "a Host this door does not answer on is refused before routing" | local_door_http::a_host_this_door_does_not_answer_on_is_refused_before_routing |
| "a cross-origin caller is refused on every route" | local_door_http::a_cross_origin_caller_is_refused_on_every_route |
| "every loopback authority is served, with or without an Origin" | local_door_http::every_loopback_authority_is_served_with_or_without_an_origin |
| "bootstrap advertises exactly the coordinator and fingerprint, uncached" | local_door_http::bootstrap_advertises_exactly_the_coordinator_and_fingerprint_uncached |
| "responses name only this door and its coordinator in connect-src" | local_door_http::responses_name_only_this_door_and_its_coordinator_in_connect_src |
| "the coordinator's own origin is admitted and answered with CORS" | local_door_http::the_coordinators_own_origin_is_admitted_and_answered_with_cors |
| "a local-network preflight from the coordinator's origin is answered" | local_door_http::a_local_network_preflight_from_the_coordinators_origin_is_answered |
| "a configured extra origin is admitted and nothing else is" | local_door_http::a_configured_extra_origin_is_admitted_and_nothing_else_is |
| "unknown paths reach the injected SPA responder; writes do not" | local_door_spa::a_deep_link_is_the_uncached_shell_and_a_write_is_refused (+ 2 SPA tests over the real responder) |
| "the terminal socket carries binary frames both ways and reports close" | local_door_sockets::the_terminal_socket_carries_binary_frames_both_ways_and_reports_close |
| "the terminal path refuses a plain request and a foreign subprotocol" | local_door_sockets::the_terminal_path_refuses_a_plain_request_and_a_foreign_subprotocol |
| "a non-loopback bind throws and never takes the port" | door_serve unit a_bind_that_is_not_the_loopback_door_is_refused_by_name (port 0 variants — never bind 4104/4114) |
| "worker config feeds the bind, admitted origins and SPA root from the env" | door_serve unit the_environment_feeds_the_admitted_origins_and_the_page_build |
| local-attachment-ui.test.ts "attachment loopback route uses its own subprotocol and handler" | local_door_sockets::the_attachment_route_uses_its_own_subprotocol_and_owner |
| "attachment loopback refuses an oversized frame before handler decoding" | local_door_sockets::an_oversized_attachment_frame_closes_the_socket_before_its_owner_reads_it |
| (v2 absent `deps.attachment` branch) | local_door_sockets::an_absent_attachment_owner_answers_404 |
| (v2 maxPayloadLength) | local_door_sockets::a_terminal_frame_over_the_payload_ceiling_is_never_read |
| (v2 server.stop(true)) | local_door_sockets::closing_the_door_ends_its_open_sockets_and_tells_their_owner |
| assignment: ungranted refused / prehello deadline | local_door_terminal::{a_hello_that_names_no_grant_is_closed_with_its_reason, a_hello_that_names_its_grant_is_answered_ready, a_socket_that_never_says_hello_is_closed_at_the_deadline} |
Commands: `/tmp/wcargo.sh test -p roost-worker --test local_door_http --test local_door_spa --test local_door_sockets --test local_door_terminal`,
`/tmp/wcargo.sh test -p roost-worker --lib door`, `/tmp/wcargo.sh test -p roost-host --lib http_security coord_config_origin`, `/tmp/wcargo.sh test -p roost-protocol --lib local_ui_door`.
Old `tests/local_door.rs` is WDoorTerm's to delete/replace (it pins the old PreHelloOwner API).

## 4. Interfaces agreed with siblings
- WDoorTerm: implements `crate::door::loopback::LoopbackHandlers for LocalTerminalSockets { type Port = local_terminal::LoopbackTerminalPacketPort; }` (their `local_terminal/loopback.rs`; must import `crate::door::{LoopbackSend, LoopbackSocket}` — `door::loopback_socket` is private; told them). Handlers never return Err. Their `on_close` must be idempotent (v2 close() calls onClose itself before port.close; the pump calls on_close again when the socket ends). Owners field `local_terminal: Arc<LocalTerminalDoor>` with `.sockets() -> Arc<LocalTerminalSockets>`; `local_terminal.dispose()` runs in `owners.shutdown()` after `door.close()`. Their WIRING also changes `WorkerOwners::build(.., worker_fingerprint: &str)`.
- WAttachDirect (wave 3): `impl LoopbackHandlers for AttachmentDirectSockets` (Port = `LoopbackAttachmentTransferPort` in attachments/direct_loopback.rs); flips `attachment: None` in `owners.loopback_routes()`; replaces door/mod.rs `LOCAL_ATTACHMENT_*` consts with `pub use roost_protocol::attachment_transfer::{..}`; reuses `tests/door_support` via `DoorOptions.attachment_owner`. Guarantees given: on_close exactly once per built port (incl. after their own `LoopbackSocket::close`); send/close never call back into handlers or block.
- W1Wire: agreed owners.rs method + boot_sequence net +1 edit and close order.

## 5. Planned mutations (each must be run and seen to fail, then reverted)
| File:line (draft numbering) | Mutation | Expected failing test |
|---|---|---|
| door/admission.rs:63 | `self.hosts.contains(host)` → `true` | local_door_http::a_host_this_door_does_not_answer_on_is_refused_before_routing |
| runtime/door_routes.rs:82 | origin check → `false` | local_door_http::a_cross_origin_caller_is_refused_on_every_route |
| runtime/door_routes.rs:143 | drop the `no-store` insert | local_door_http::bootstrap_advertises_…_uncached |
| runtime/door_routes.rs:243 | `secured` never stamps | local_door_http::responses_name_only_this_door_… |
| runtime/door_routes.rs:92 | 404 → 400 | local_door_sockets::an_absent_attachment_owner_answers_404 |
| runtime/door_routes.rs:~187-193 | skip the subprotocol check | local_door_sockets::the_terminal_path_refuses_… (owner sees a socket) |
| runtime/door_routes.rs:221-222 | remove max_message_size/max_frame_size | local_door_sockets::a_terminal_frame_over_the_payload_ceiling_is_never_read |
| door/spa.rs:54 | IndexFallback → NotFound | local_door_spa::a_deep_link_is_the_uncached_shell_… |
| door/spa.rs:124 | index rule → "no-cache" | local_door_spa::a_deep_link_is_the_uncached_shell_… |
| door/loopback_socket.rs:89 | `before == 0` → `true` | door::loopback_socket unit test |
| door/loopback.rs:198 | deliver text frames as bytes | local_door_sockets::the_terminal_socket_carries_… |
| door/loopback.rs:181 | remove the door_stopped branch | local_door_sockets::closing_the_door_ends_… |
| runtime/door_serve.rs:56-59 | exact loopback → `address.ip().is_loopback()` | door_serve unit a_bind_that_is_not_the_loopback_door_… (127.0.0.2) |
| runtime/door_serve.rs:240 | drop `.map(str::trim)` | door_serve unit the_environment_feeds_… |
| door/admission.rs:111 | drop the `extra` chain | local_door_http::a_configured_extra_origin_… |
| roost-host http_security.rs:22 | drop dedupe | http_security::connect_sources_are_deduplicated_in_first_seen_order |
Known masked pair: door/loopback.rs attachment size check vs `max_message_size` (both 1 MiB, exactly as v2's explicit check vs Bun's maxPayloadLength) — removing one alone does not fail `an_oversized_attachment_frame…`; removing both does. Report it as such.

## 6. Consumers of new values (production readers)
`LoopbackSend` → WDoorTerm's/WAttachDirect's port adapters; `LoopbackRoute::{name,subprotocol}` → door_routes::upgrade + pump logs; `LoopbackRoutes.attachment` → door_routes fetch (None = 404); `DoorServer` → boot_sequence (close at shutdown, origin in the stopping log); `DoorConfig` → LocalDoor::serve; `DoorAdmission::{admits_host,admits_origin,cors_origin,connect_origins,browser_origin_count}` → door_routes / door_serve; `browser_origin` → door/admission.rs; `security_headers` → door_serve; `WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV` → DoorConfig::from_env; `LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES` → pump.

## 7. Open items / parity notes for the report
- roost-coord `middleware/security.rs` keeps a private `build_csp` duplicating `roost_host::http_security` (v2 had one builder); worker may not edit roost-coord → coord track should switch to `roost_host::http_security::build_csp`.
- roost-cli `services/service_environment.rs:39` `ENV_WORKER_LOCAL_UI_ALLOWED_ORIGINS` duplicates the new protocol constant (cli track).
- roost-coord `http/spa.rs` answers `/index.html` requested by name with the index cache rule; v2 `spa.ts` gives "no-cache" (worker adapter follows v2). Coord `spa_cache` keeps stale per-state entries; v2/worker keep one entry per path.
- Written/Queued approximation: Bun reports >0 when its kernel write succeeds; the Rust socket says Written only when nothing unflushed is queued, so bursts report Queued earlier (port turns that into "backpressured" = still sent; refused only past 4 MiB accumulated).
- Idle ping/close (Bun default 120 s) is implemented but untested (would take 240 s).
- `LocalDoor::serve` is sync and must run inside the tokio runtime (it spawns).
- Compile risks to check first: closure param annotations in door/loopback.rs `for_route`; `format_args!` passed as `impl Display` in door_routes::refused calls; `if precompressed && let Some(..)` (edition 2024 let-chains, OK on rust 1.98).

## 8. Commit message draft
`worker: the loopback door serves its routes — Host/Origin gate, bootstrap, socket upgrades, SPA`
Body: ports v2 local-ui-server.ts (+ server half of boot-local-terminal.ts, http-security.ts, spa.ts responder use); one pump for both loopback routes with v2's guarded/close semantics; attachment path answers v2's absent-owner 404 until W-ATTACH; list the mutations (§5), consumers (§6), cross-owner edits (roost-protocol local_ui_door, roost-host http_security + coord_config_origin + lib.rs, worker Cargo.toml/Cargo.lock).
Paths: crates/roost-protocol/src/local_ui_door.rs, crates/roost-host/src/{lib.rs,http_security.rs,coord_config_origin.rs}, crates/roost-worker/Cargo.toml, Cargo.lock, crates/roost-worker/src/door/{mod,admission,loopback,loopback_socket,spa,spa_cache}.rs, crates/roost-worker/src/runtime/{mod,door_routes,door_serve,owners,boot_sequence}.rs, crates/roost-worker/tests/{door_support/mod.rs,local_door_http.rs,local_door_spa.rs,local_door_sockets.rs,local_door_terminal.rs}.
