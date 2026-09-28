protocol: the local UI door's paths, subprotocol, payload limits and allowed-origins variable

The worker's loopback door (WDoorHttp/WDoorTerm) and the browser bundle must agree on the
local terminal socket path and subprotocol, the bootstrap path, the client frame ceiling
and the backpressure ceiling (v2 packages/protocol/src/local-ui-door.ts and the worker's
local-door/ constants; the allowed-origins variable is v2 apps/worker/src/host/config.ts:97-98).
They live in roost-protocol so the web client reads the same literals it is served.

Self-contained (roost-protocol local_ui_door.rs only). Cherry-pick target: web track.

Production consumers:
- LOCAL_TERMINAL_PATH, LOCAL_TERMINAL_SUBPROTOCOL, LOCAL_BOOTSTRAP_PATH <- roost-worker
  door routes (door/, runtime/door_serve.rs).
- LOCAL_TERMINAL_MAX_PAYLOAD_BYTES, LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES <- roost-worker
  local_terminal socket carrier.
- WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV <- roost-worker door Origin gate (comma list, v2
  ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS).
Guards: exercised by the worker door tests (local_door, door_* bins) in the WDoorHttp commit.
