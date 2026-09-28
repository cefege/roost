worker: the loopback door serves its routes — Host/Origin gate, bootstrap, socket upgrades, SPA

The local door was bound but answered nothing. It now serves v2's routes on
the bound listener from the moment the owners exist until shutdown:

- runtime/door_serve.rs: LocalDoor::serve spawns axum::serve with graceful
  shutdown; DoorServer::close (and Drop) stops the listener and every
  upgraded socket (v2 server.stop(true)). The bind accepts exactly
  127.0.0.1 / [::1] (v2 parseLoopbackBind). DoorConfig reads
  ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS (comma split, trimmed, blanks
  dropped) and ROOST_WEB_DIST_PATH (v2 host/config.ts). Tests bind port 0.
- runtime/door_routes.rs: one fetch in v2's order: Host 403 host_not_local,
  Origin 403 origin_not_local, bootstrap (OPTIONS 204 + private-network
  preflight, GET/HEAD 200 no-store JSON + CORS, else 405), the terminal
  upgrade, the attachment path (absent owner = v2's 404
  attachment_unavailable), 405 for writes, then the SPA. Upgrades refuse
  {route}_method / _subprotocol / _not_upgradable; 1 MiB frame ceiling.
  Every non-101 answer carries the shared security headers.
- door/admission.rs: Host/Origin allowlists, connect-src origins.
- door/loopback.rs + loopback_socket.rs: one pump for both routes with v2's
  guarded (1011), non-binary drop, attachment 1 MiB close (1009), Bun's idle
  ping/close, and on_close once per socket; LoopbackSocket send reports
  Written/Queued/Dropped like Bun ws.send.
- door/spa.rs + spa_cache.rs: the disk-build responder over
  roost_host::spa_path with v2 spa.ts cache rules and a gzip memo.
- owners.rs loopback_routes(): terminal = LocalTerminalSockets (WDoorTerm),
  attachment = None until WAttachDirect. boot_sequence: serve after owners
  are built; shutdown closes the door before owners.shutdown() (v2 close()).

v2 ported: apps/worker/src/local-door/local-ui-server.ts, the server half of
apps/worker/src/boot/boot-local-terminal.ts, packages/host/src/http-security.ts,
the worker's use of packages/host/src/spa.ts.

Cross-owner edits: roost-protocol local_ui_door.rs (paths, subprotocol,
payload/backpressure limits, ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS);
roost-host http_security.rs (new, v2 applySecurityHeaders), lib.rs (mod +
browser_origin re-export), coord_config_origin.rs (browser_origin, ws/wss/ftp
default ports).

Consumers: LoopbackSend -> local_terminal/loopback.rs port adapter;
LoopbackRoutes.attachment -> door_routes fetch; DoorServer -> boot_sequence;
DoorConfig -> LocalDoor::serve; browser_origin -> door/admission.rs;
security_headers -> door_serve; WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV ->
DoorConfig::from_env; LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES -> the pump.

Replaced arms: none in link_drain/downstream (the attachment path's
absent-owner 404 lives in the door, as in v2).

Mutations (each seen to fail, then reverted): see MUTATIONS below.

MUTATIONS (run, seen to fail, reverted; tree verified identical to pre-mutation by md5):
- roost-host http_security.rs:22 dedupe -> `if true` => connect_sources_are_deduplicated_in_first_seen_order
- door/admission.rs:63 host check -> true => a_host_this_door_does_not_answer_on_is_refused_before_routing
- door/admission.rs:111 drop extra-origin chain => a_configured_extra_origin_is_admitted_and_nothing_else_is, admission::only_exact_origins_are_admitted
- runtime/door_routes.rs:143 drop no-store => bootstrap_advertises_exactly_the_coordinator_and_fingerprint_uncached
- runtime/door_routes.rs:242 secured never stamps => responses_name_only_this_door_and_its_coordinator_in_connect_src
- runtime/door_serve.rs:55 exact loopback -> is_loopback() => door_serve::a_bind_that_is_not_the_loopback_door_is_refused_by_name
- runtime/door_serve.rs:240 drop trim => door_serve::the_environment_feeds_the_admitted_origins_and_the_page_build
- door/loopback_socket.rs:89 before==0 -> true => loopback_socket::a_send_behind_unflushed_bytes_is_queued_and_a_closed_socket_drops
- batch B (door_routes.rs:92 404->400; :189 skip subprotocol; :221-222 drop max sizes; loopback.rs:195 deliver text; :181 drop door_stopped; spa.rs:54 IndexFallback->404) => an_absent_attachment_owner_answers_404, the_terminal_path_refuses_..., a_terminal_frame_over_the_payload_ceiling_is_never_read, the_terminal_socket_carries_..., closing_the_door_ends_..., a_deep_link_is_the_uncached_shell_... (the_attachment_route_uses_its_own_subprotocol_and_owner also failed: text delivery mutation)
- masked pair removed together (door_routes max sizes + loopback.rs:232 attachment check) => an_oversized_attachment_frame_closes_the_socket_before_its_owner_reads_it (+ terminal ceiling test)

Open: batch C (door_routes.rs:82 origin check -> false; spa.rs:124 index rule -> "no-cache")
and the single-sided masked-pair check (D1) are NOT RUN — open item in the handoff.

Red classified at resume: roost-host v3_install_identity forbids the older-generation token in
roost-host sources; http_security.rs's header cites the TypeScript module by path instead.

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
