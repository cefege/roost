# U-1 pump / smoke map (worktree /home/almalinux/repos/roost-v3-web; all paths relative)

> Not written to docs/v3-handoff/u1-pump-smoke-map.md: this read-only subagent has no write tool. Save this block verbatim.

## A. v2 BOOT PATH (browser → authorized, synced)

**A1 Entry and fragment capture**
- `apps/web/src/entry.ts:10` calls `captureAndScrubFragmentCredential()` before any network module. `entry.ts:17` does `loadLocalBootstrap().then(()=>import("./main.tsx"))`.
- `client/auth/fragment-credential.ts:131-139` parses `#pair=<t>`; exactly one non-empty value is required, otherwise the result is `invalid`.
- `:155-179`: `history.replaceState` strips `pair` from query and hash (`:163-167`). The token is kept in module memory and in `sessionStorage["roost.fragmentCredential.v1"]` (`:28,108-115`).
- `peekCapturedFragmentCredential` is at `:181-184` and `clearCapturedFragmentCredential(kind)` at `:190-197`.
- `main.tsx:151-176` (`mountApp`): `claimTabIdentity()` → `startLocalTerminalFastPath()` → `render(<App/>)`.
- `App.tsx:64` calls `bootstrapSync()`.

**A2 Bootstrap order** (`store/sync-bootstrap.ts:171-271`, `_bootstrap`)
1. `claimTabIdentity()` (`:172`).
2. `publicCoordClient.authCoordIdentity({})`, which carries no JWT (`:186`, `client/rpc/connect.ts:151-163`). On success it sets `coord_identity` (`:197-201`).
3. `_dispatchCapturedFragmentCredential()` (`:204`, `sync-bootstrap.pair.ts:24-51`).
   - `peek` → `redeemPairToken(token)`.
   - On ok: `clear("pair")` and `location.replace("/")` (`:39-40`), then return. The reload re-boots as a paired key.
   - On failure: clear only if the error is authoritative (`:35`), log `pair.redeem_failed`, and continue.
4. Redeem RPC `store/auth/redeemPairToken.ts:30-55`: `coordClient.authRedeemBrowser({ token, sshPubkeyB64: getPublicKeyB64(), label: browserSelfLabel() })`.
   - Proto: `protocol/proto/roost/v1/coordinator.proto:638` `AuthRedeemBrowserRequest{token=1, ssh_pubkey_b64=2, label=3}`; rpc at `:1006`.
   - Authoritative codes: InvalidArgument, AlreadyExists, PermissionDenied, Unauthenticated (`redeemPairToken.ts:45-50`).
   - The call goes through `coordClient`, so it carries the device JWT.
5. `_startSyncLoop()` (`:206`), then `waitForSyncSubscribed(3000)` (`:207`, `SYNC_SUBSCRIBED_WAIT_MS` at `:37`).
6. If no subscribed frame arrives: probe `coordClient.sessionsList({})` (`:211`).
   - A `device` classification leads to `markBrowserDeviceRejected("bootstrap_probe")` (`:215-224`).
   - Otherwise it schedules a retry with backoff `min(1000·2^n, 10s)` (`:159-169,225-227`).
7. If subscribed: install the hydrators once (`:229-266`).

**A3 Web key** (`client/auth/web-key.ts`)
- Ed25519 WebCrypto key, non-extractable (`:103-105`), stored in IndexedDB via `web-key-storage.ts`, created under the Web Lock `roost-web-key-v1` (`:18,91-97`).
- `getPublicKeyB64` is standard base64 of the raw key (`:113-116,207-214`).
- JWT (`:118-131`):
  - header `{alg:"EdDSA",typ:"JWT",kid:<sha256-hex fp>}`
  - payload `{sub:kid,aud:"roost-coordinator",iat,exp:iat+300}`
  - base64url signature
  - cached for 240s (`:22-23,221-233`)

**A4 Connect header format** (`client/rpc/connect.ts`)
- The interceptor sets `Authorization: Bearer <jwt>` (`:117-132`, set at `:121`). A signing failure is only signalled; the request still goes out.
- Every call sets `x-roost-tab-id: <tabId>` (`:129`, header constant `packages/protocol/src/wire/headers.ts:8`).
- Transport: `createConnectTransport({baseUrl, useBinaryFormat:true})` (`:135-145`). Base URL is same-origin unless a local-bootstrap or self-hosted override applies (`:73-107`).
- `classifyAuthFailure(err, path)` returns `"device"` only when all three hold (`:40-57`):
  - the code is `Unauthenticated`,
  - the `x-roost-auth-layer: device` header is present (`headers.ts:13,16`),
  - the path is in `DEVICE_AUTH_REQUIRED_PATHS` (`:26-36`: WorkersList, SessionsList, WorkspacesList, TasksList, McpList, DevicesList, DevicesRevoke, DevicesRotateCurrent, PairApprovalStatus).

**A5 Access state** (`store/root.ts:38` `"checking"|"authorized"|"unauthorized"`, initial value `"checking"` at `:93`)
- **checking**: initial value, and the credential-boundary reset (`root.ts:125`).
- **unauthorized**: `markBrowserDeviceRejected(source)` (`store/browser-access.ts:20-29`). Triggers:
  - bootstrap probe (`sync-bootstrap.ts:222`)
  - terminal hydration failure classified as `device` (`:240`)
  - workers-refresh `device` (`:79-85`)
  - Sync WS close code **4001** (`sync.ts:104,256-260`, registered at `sync-bootstrap.ts:31`)
- **authorized**: `markProtectedSnapshotPublished()` (`browser-access.ts:32-43`). Called only from `onTerminalSnapshotApplied` (`sync-bootstrap.ts:252`), which runs from the TERMINAL hydrator's `apply` (`sync-bootstrap-hydration.ts:80-85`). Success of any other RPC never grants access (`sync-bootstrap.ts:74-78`).
- Gate: `App.tsx:143-153` renders `AccessCheckingScreen` / `Onboarding` / children.

**A6 Sync dial** (`store/sync.ts:112-117,171-265`)
- URL: `ws(s)://<coordBase>/ws/coord-sync?since=<lastSeenEventId>&tab=<encodeURIComponent(tabId)>&flow=1&sync_v=2`.
  - Constants in `packages/protocol/src/wire/sync-ws.ts:6-11`: `SYNC_WS_PATH="/ws/coord-sync"`, `SYNC_AUTH_SUBPROTOCOL="roost-auth"`, `FLOW_V1="1"`, `V2="2"`.
  - `since` comes from `sync-frame.ts:51-55`.
- Credential: `new WebSocket(url, ["roost-auth", jwt])` (`sync.ts:182-189`). The JWT is never in the URL. `binaryType="arraybuffer"`.
- OPEN only arms the stale watchdog. "OPEN is not hydration readiness" (`:211-229`).
- Backpressure close is `1013 "sync backpressure"` (`client/sync/sync-flow.ts:68-70`).

**A7 SyncSubscribed** (`store/sync-inbound.ts`)
- The first frame must be `subscribed`, else the link fails (`:55-60`).
- `handleSubscribed` (`:82-128`) requires socketId, processEpoch, and all 7 domains with generation > 0 (`:42-50,88-102`). It builds `link.v2` and resolves the subscribed waiters (`:124`).
- Per domain: if a lazy hydrator exists (AUDIT), activate it; else if the coordinator says `subscribed`, trigger hydration (`:125-127`).
- Control frames have `deliverySeq==0`, domain UNSPECIFIED, generation 0 (`:206-214`).

**A8 Client domainSubscribe / hydration per domain**
- `domainSubscribe{domain,generation}` is sent ONLY for lazy domains (AUDIT), via `_activateLazySyncDomain` (`sync-domain-hydration.ts:70-86`). Unsubscribe is at `:52-63`.
- Default domains are already `subscribed` in the announcement. The client just runs the hydrator: 15s deadline (`:33,95-106`), retry `min(500·2^n,10s)` (`:128-149`).
- Hydrators (`sync-bootstrap-hydration.ts`):
  - TERMINAL: `sessionsList({syncSocketId: token.socketId})` (`:54-89`). A missing `syncSnapshotToken` forces a reconnect (`:62-66`). Proto field: `coordinator.proto:117-124` `sync_socket_id=3`, response `sync_snapshot_token=2` (`:133-136`).
  - WORKERS: `workersList({})` (`:91-129`). Sets `workers` and `routableFps`; this is what feeds `__smoke.state().workers`.
  - WORKSPACES: `workspacesList` (`:131-149`).
  - TASKS: `tasksList` (`:151-160`).
  - MCP: `mcpList` (`:162-170`).
  - PAIR: `pairList` (`:172-199`).

**A9 domain_ready** (`store/sync-domain-state.ts:104-131`)
- After `snapshot.apply()`, send `SyncClientFrame{socketId, command: domainReady{domain, generation: token.domainGeneration, snapshotToken}}` (`:89-102`).
- Then set `domain.ready=true`; for TERMINAL, notify the generation (`:127-129`).
- Proto: `sync.proto:134-137` `SyncDomainReadyCommand{domain=1, generation=2, optional snapshot_token=3}`; `SyncClientFrame` at `sync.proto:304-314` (`ack_delivery_seq=1`, `domain_subscribe=2`, `domain_ready=4`, `input=6`, …).
- The coordinator never sends a domain_ready frame. Server controls are `subscribed=40` and `domain_reset=41` (`sync.proto:290-291`).

**A10 Frames flow** (`sync-inbound.ts:173-204`)
- Frames with the wrong generation or an unsubscribed domain are ignored.
- An application frame on a subscribed domain that is not yet ready throws, which closes the link.

**A11 ACK cadence** (`client/sync/sync-flow.ts:44-65`)
- After each successfully dispatched sequenced frame (`deliverySeq>0`) on the current, accepting, OPEN link, send `SyncClientFrame{ackDeliverySeq: frame.deliverySeq, socketId}`.
- That is one cumulative ACK per applied frame, with no batching. Controls are never ACKed.

## B. RUST SIDE (same path) — what exists / event↔effect / missing

**B1 Entry**
- `crates/roost-web/src/main.rs:14-23`: `install_tracing` → `capture_and_scrub()` → log → `dioxus::launch(App)`.
- `platform/fragment_credential.rs:138-166` ports the scrub and stores the token in `sessionStorage["roost.fragmentCredential.v1"]` (`:23`).
- `captured_credential()` (`:169`) and `clear_captured_credential()` (`:174`) have **no callers**.

**B2 App**
- `lib.rs:85-92` does `provide_context(Rc<RefCell<ClientCore>>)`.
- `build_core()` (`:101-107`) uses BrowserClock, LocalStorageKeyValueStore, and `tab_id()`.
- `tab_id()` is `localStorage["roost.tabId"]`, `tab-<hex>` (`:72,116-127`). This differs from v2's sessionStorage + Web Lock identity.

**B3 Gate** (`app.rs`)
- `GatedApp` (`:136-146`) reads `core.borrow().store().browser_access_state` during render (`:206-208`).
- Maps via `Gate::for_state` (`:108-114`) → `access_gate::CheckingScreen` (`components/access_gate.rs:27`, testid `access-checking`) / `UnauthorizedScreen` (`:45`, testid `access-unauthorized`) / `AuthorizedShell`.
- `app.rs:124-133` states outright: "Nothing drives `ClientCore::handle` in this build — the host pump … is not here … the gate holds at `Checking`".

**B4 BrowserAccessState** (`roost-client-core/src/store/root.rs:27-36`): variants `Checking` (default), `Authorized`, `Unauthorized`. Writers:
- `Store::new` sets Checking (`store.rs:196`).
- `set_browser_access_state` (`root.rs:77-85`) has **no production caller**. Grep finds only its definition and `tests/store_revision.rs:253,358`.
- `clear_account_state_for_logout` resets to Checking (`root.rs:131-155`). It has no caller in src.
- Nothing sets Authorized on a SessionsList result (`handle_sync.rs:218-236`), or Unauthorized on a 4001 close. That close only latches `sync.auth_revoked` (`sync/link.rs:274-286`, `handle_event.rs:90-104`).

**B5 Core API**
- `core.rs:95-105`: `ClientCore::handle(ClientEvent)->Vec<Effect>`, ordered effects.
- `store()`/`store_mut()`/`storage()` at `:108-126`.

**B6 Event → effect mapping** (`handle_event.rs`)
- `DialRequested` → `Effect::DialSync{generation, dial}` (`:47-58`), refused if `auth_revoked`.
  - `SyncDial::for_tab` (`sync/link.rs:130-160`) carries path `/ws/coord-sync`, subprotocol `roost-auth`, flow `1`, sync_v `2`, tab_id, since.
- `BootstrapRequested` → `Rpc(CoordIdentity)`, `Rpc(SessionsList)`, `Rpc(WorkersList)` all at once (`:59-73`).
  - v2 differences: this happens BEFORE subscribed; `RpcCall::SessionsList` has only `call_id`, so there is no `sync_socket_id` (`effect.rs:229-232`) and no snapshot token will be issued.
- `SyncLinkOpened{generation, socket_id, process_epoch}` → `sync.open_link` (`:76-94`, `link.rs:247-268`).
  - socket_id and process_epoch exist only in the `subscribed` frame, so the host must decode `subscribed` before it can emit this.
- `SyncFrameReceived{generation, delivery_seq, frame}` → `handle_sync_frame` (`handle_sync.rs:43-93`).
  - Non-control frames are retained until `store.hydrated` (`:55-62`).
  - ACK is `SendSync(Ack{ack_delivery_seq, socket_id})` per applied sequenced frame (`:85-92`). This matches v2's cadence.
- `apply_frame` (`handle_sync/apply_frame.rs`):
  - `Subscribed` → `install_subscribed` and `SendSync(Subscribe{domain})` for every domain not already subscribed (`:21-33`). v2 subscribes only lazy AUDIT; this is a divergence.
  - `DomainReset` → `HydrateDomain{domain, generation}` if still subscribed (`:70-100`).
  - Inbound `SyncFrame::DomainReady` → `SendSync(DomainReady{domain, snapshot_token})` (`:34-68`). The coordinator never sends DomainReady (A9), so the host must synthesize this after hydration. `SyncCommand::DomainReady` and `Subscribe` also lack the `generation` field the proto needs (`effect.rs:127-142`); an encoder must fill it from `store.sync.domain_generation`.
  - CellGrid / CellGridChunk → `store.terminal_mut_if_present(sid).admit_frame|admit_chunk`, which bumps `frame_revision` and `note_change` (`:115-146`).
- `HydrationCompleted` → `hydrate()`: `hydrated=true` and drain the retained frames (`handle_sync.rs:205-214`).
- `RpcResultReceived` (`handle_sync.rs:217-257`):
  - `CoordIdentity` sets `account_id`.
  - `SessionsList` → `sessions.apply_snapshot` + `issue_snapshot_token(Terminal)`, but does NOT set Authorized.
  - `WorkersList` and `PairTokenRedeemed` are only logged (`:253-255`). `store::mutations::replace_workers` (`store/mutations.rs:84`) has no src caller, so `store.workers` stays empty.
  - `Failed{call_id, message}` carries no Connect code or auth layer, so `classify_auth_failure` (`client/rpc/auth_failure.rs:70`, uncalled) cannot run.
- `ViewOpened` → `store.terminal_mut(sid, fp)` creates the replica (`handle_terminal.rs:54`).

**B7 Missing pieces (none exist anywhere in `crates/roost-web/src` or `crates/roost-client-core/src`)**
- No ClientEvent for a captured fragment credential (`event.rs:24-219`). `RpcCall::RedeemPairToken{call_id, token}` (`effect.rs:239-244`) lacks `ssh_pubkey_b64` and `label`, and nothing emits it; only `methods.rs:50` names it `AuthRedeemBrowser`.
- No host executor for ANY Effect: `DialSync`, `CloseSyncLink`, `SendSync`, `SendDirect`, `Rpc`, `SignChallenge`, `PersistWatermark`, `PersistAgentSeen`, `RequestDirectGrant`, `HydrateDomain` (`effect.rs:19-99`). Grep finds no `Effect::`, `.handle(`, `WebSocketSyncSocket::new` or `FetchConnectTransport::new` in roost-web src.
- No FirehoseFrame → `SyncFrame` decoder. The core says "host decodes protobuf" (`sync/inbound.rs:4-7`), and no client-side decoder exists.
- No `SyncCommand` → `SyncClientFrame` encoder, and no RpcCall ⇄ proto body codec (`client/rpc/request.rs:14` only holds bytes).
- The device key is in flight and not compiled: `platform/device_key/{lifecycle.rs,bearer_cache.rs,schema.rs}` exist, but `platform/mod.rs:19-26` does not declare `device_key`, and no IndexedDB/WebCrypto adapter file exists. Core has `DeviceKeyManager` (`client/auth/device_key.rs:81-205`) and JWT builders (`jwt.rs:32-51,138`).
- No sidebar `folder-list` testid (grep of the roost-web layout finds none), no `error-boundary`, no `__smoke`. The `smoke` feature exists but is empty (`crates/roost-web/Cargo.toml:19-23`).

**B8 Transports that exist but are unwired**
- `platform/sync_socket.rs`:
  - `sync_socket_url` (`:105-146`) renders `ws(s)://host/ws/coord-sync?since&tab&flow&sync_v`.
  - `WebSocketSyncSocket::open` (`:242-…`) passes subprotocols `[dial.subprotocol, bearer]`.
  - `SyncSocketHandle::{drain,send,close}` (`:178-212`); the inbox is bounded at 512 (`:38`).
  - `SyncSocketMessage::{Open,Binary,Closed}`.
- `platform/rpc.rs`:
  - `FetchConnectTransport` POSTs `<base>/roost.v1.CoordinatorService/<Method>` (`:148-153`).
  - Headers (`:155-178`): `content-type: application/proto`, `connect-protocol-version: 1`, `x-roost-tab-id`, `authorization: Bearer <jwt>`.
  - Returns the whole response with the `x-roost-auth-layer` header (`:185-235`).

**B9 Minimal pump the U-1 host must add** [INFERENCE from the above]
1. Resolve the device key and bearer.
2. If a fragment credential was captured, run the AuthRedeemBrowser RPC (token, pubkey, label), then clear it and `location.replace('/')`.
3. Public AuthCoordIdentity (no bearer).
4. `handle(DialRequested)` → open the socket.
5. On the `subscribed` frame, emit `SyncLinkOpened` then `SyncFrameReceived(Subscribed)`, and suppress the non-AUDIT Subscribe sends.
6. SessionsList with `sync_socket_id` → `RpcResultReceived` → set Authorized.
7. Send `domain_ready{Terminal, gen, token}`.
8. WorkersList → `replace_workers`, then `domain_ready{Workers}`; the other domains similarly (no token).
9. `HydrationCompleted`, then perform `SendSync` Acks.
10. Handle 4001 or a `device` auth failure → Unauthorized.

## C. PLAYWRIGHT SMOKE FIXTURE REQUIREMENTS

**Config** (`playwright.config.ts:12-78`)
- testDir `smoke/terminal`, fullyParallel, workers `max(2, min(4, cores/2))`, retries 0, timeout 120s.
- Projects: chromium-desktop (and webkit-iphone on darwin), firefox-peer (peer specs only), chromium-serial (`@serial`), tv (`@tv`).
- No webServer: each worker boots its own stack.

**Stack lifecycle** (`smoke/terminal/stack.ts:66-…`)
- The worker-scoped fixture `stack` calls `startTerminalTestStack()` (`fixtures.ts:311-319`).
- Root is `mkdtemp("/tmp/roost-terminal-system-")` (`stack.ts:73-74`), with `home`, `coord.db`, and per-child data and tmp dirs under it (`:75-116`).
- It prints `smoke stack: coordinator=… worker=… web=…` (`:167`, `stack-executables.ts:167-172`).

**Coordinator launch**
- `stack-coordinator.ts:68-107` binds `127.0.0.1:0` (ephemeral) and learns the port from the JSON log `listening` line (`:115-129`); `baseUrl = http://127.0.0.1:<port>`.
- The command is `bun apps/coord/src/main.ts`, or `<ROOST_SMOKE_COORD_EXECUTABLE> coord` (`stack-runtime.ts:223-231`).
- Env (`stack-runtime.ts:268-296`): `ROOST_COORDINATOR_BIND`, `ROOST_TRUST_PROXY=0`, `ROOST_RELAXED_CSP=1`, `ROOST_COORDINATOR_DB`, `ROOST_COORDINATOR_AUTHORIZED_KEYS=<root>/authorized_keys.roost`, **`ROOST_WEB_DIST_PATH = resolveSmokeWebDist() ?? <repo>/apps/web/dist`** (`:280-281`), `ROOST_GIT_SHA`, `ROOST_CORS_ALLOWED_ORIGINS=<reserved local-UI origins>`.

**ROOST_SMOKE_WEB_DIST**
- Defined at `stack-executables.ts:21`. It is resolved to an absolute path and must be a directory containing `index.html` (`:103-117,142-148`).
- The coordinator serves it same-origin at `/`:
  - TS: `apps/coord/src/main.ts:113` `createSpaResponder(cfg.webDistPath)` with `packages/host/src/config.ts:55`.
  - Responder (`packages/host/src/spa.ts:89-178`): disk wins; exact asset path if the file exists; `assets/*` 404s when missing; every other path falls back to `index.html` with no-cache; `assets/*` is immutable. `.wasm` is served as `application/wasm` (`spa.ts:25`) and CSP allows `'wasm-unsafe-eval'` (`http-security.ts:18`). A dx bundle is therefore servable [INFERENCE: its hashed files must sit under `assets/` or at the root].
  - Rust coord: `crates/roost-coord/src/serve.rs:165-174` `SpaMount::from_dist_path` with the same env (`roost-host/src/coord_config_loader.rs:35,123`).

**API key and worker**
- API key: `loadWorkerKey(<root>/api.key)` → `authorizeTerminalTestApiKey` (sqlite write into coord.db) → `buildAuthorizedApiClient` (`stack.ts:218-226`).
- Worker: `bun apps/worker/src/main.ts`, or `<ROOST_SMOKE_WORKER_EXECUTABLE> worker` (`stack-worker-runtime.ts:81-86`).
  - Env (`:94-118`): `ROOST_COORDINATOR_URL`, `ROOST_BOOTSTRAP_TOKEN` (from `authMintBootstrap{kind:"worker"}`, `stack.ts:245`), `ROOST_WORKER_LABEL`, `ROOST_WORKER_DATA_DIR`, `ROOST_WORKER_KEY_PATH`, `ROOST_KEEPER_QUIET=1`, and `ROOST_WORKER_LOCAL_UI_BIND=127.0.0.1:<reserved ephemeral>` (`stack-local-ui.ts:1-6,76-90`; this avoids the 4104 default).
  - `waitForTerminalWorkerRoutable` (`stack.ts:264`).

**Child env isolation** (`stack-runtime.ts:32-47`): strips every inherited `ROOST_*` and sets `HOME=<root>/home`, `TMPDIR/TMP/TEMP=<per-child tmp>`. The keeper endpoint is resolved from the worker dataDir (`stack-runtime.ts:128-131`).

**Page setup** (`fixtures.ts:185-275`)
- New context; addInitScript sets `localStorage.roostSmoke="1"`, `roost.whatsNew.lastSeenVersion="2.0.0"`, and seeds sidebar `folders`/uncollapsed (`:196-204`).
- `enrollSmokeBrowser` (`:142-183`), one 90s deadline (`:55`):
  1. `client.authMintBootstrap({kind:"browser", label:"roost-terminal-test-browser"})`.
  2. `goto(${baseUrl}/#pair=${token})`.
  3. Wait for `location.hash===""`.
  4. Wait for `window.__smoke?.state().workers[stack.workerFp]` truthy (`:67-79`).
  5. `.workbench-shell[data-compact]` must be `"true"|"false"` (`:159-168`).
  6. `getByTestId("folder-list")` count 1, visible when not compact (`:170-178`).
  7. `getByTestId("error-boundary")` count 0 (`:180-182`).
  - Then it waits for the expected worker fps again (`:252`).
- Teardown (`:255-262`): `evaluate(() => { __smoke?.forceVisible(false); await __smoke?.cleanupCreated(); })`, errors swallowed, then `context.close()`. Stack logs are attached on failure (`:113-124`).
- v2 producers of the DOM contract: `components/layout/AppShell.tsx:125` (`.workbench-shell data-compact`), `components/sidebar/FolderList.tsx:308` (`folder-list`), `components/AppErrorBoundary.tsx:51`.
- Rust today renders `.workbench-shell data-compact` (`crates/roost-web/src/components/layout/app_shell.rs:85-87`), but only when Authorized, and has no `folder-list`.

**live-stack** (`smoke/terminal/live-stack.ts:1-48`)
- Same `startTerminalTestStack`; prints `READY <baseUrl> worker=<fp>`, and with `--pair` prints `PAIR <baseUrl>/#pair=<token>`.
- It requires a `VITE_ROOST_SMOKE=1` dist, or ROOST_SMOKE_WEB_DIST pointing at a smoke-built Rust dist.
- **Beside a production v2 install:** yes [INFERENCE from the code].
  - Ports are ephemeral: coord `127.0.0.1:0`, local-UI reserved via `listen(0)`, never the fixed 4104 default.
  - Everything lives under a fresh `/tmp/roost-terminal-system-*`, with HOME and TMPDIR overridden and `ROOST_*` stripped.
  - The keeper is per-dataDir.
  - Stop only touches its own children and `rm -rf` of its own root (`stack.ts:128-162`).
  - Caveat: `ensureSmokeStackBinary` may build Rust binaries (`stack-coordinator.ts:80`).

## D. window.__smoke CONTRACT

**Install**
- `App.tsx:73-76`: only if `import.meta.env.VITE_ROOST_SMOKE==="1"` (build-time fold) AND `localStorage.roostSmoke==="1"`. Dynamic `import("./smoke/smoke.ts")` → `maybeInstallSmokeBackdoor()` (`smoke.ts:35-120`) → `window.__smoke=api` (`:118`). The Rust install site is `crates/roost-web/src/smoke/backdoor.rs`.
- The Rust equivalent must be behind feature `smoke` (`crates/roost-web/Cargo.toml:19-23`).
- The spec-side type is `smoke/terminal/terminal-smoke-api.ts:29-33` (`Window.__smoke: SmokeApi`).

**state()** (`smokeRuntimeControls.ts:60-67`): `{sessions:{...rootStore.sessions}, workspaces, workers, pair_requests}`. Values are wire shapes (`smokeTypes.ts:188-193`): `Record<id, Session|Workspace|Worker>`. The fixture reads `workers[fp]`, `sessions[id].cwd`, `.status`, and `.custom_title`.

**Methods.** Format: name — semantics — impl — spec/helper call sites (grep of `smoke/terminal`; F = files; "~" = paginated count).

Input and render probes:
- input(sid, text) — bytes via terminal transport, bypasses textarea — `smokeTerminalInputController.ts:58` (→ `sendTerminalInput`, `store/transport/sync-outbound.ts:311`) — ~30F
- terminalInputCapture() — admitted batches + outcomes — `:64` — 4 sites
- resetTerminalInputCapture() — `:74` — 4
- paneFocused(sid) — slot has textarea and it is activeElement — `smokeTerminalRenderProbes.ts:24` — ~13
- viewportText(sid) — slot textContent — `:33` — ~15
- renderProbe(sid) — `.cell-grid` scroll geometry, rows — `:37` — ~20 (in ~40F with markerScan/viewportText)
- paintedScrollback(sid) — renderer paintPresentation — `:73` — ~10
- hasPaintedScrollbackRange — `:82` — 2
- paintedScrollbackRange — `:85` — 1
- markerScan(sid, prefix) — `:88` — ~30
- terminalDimensions(sid) — `--cell-cols` and viewport row count — `:141` — 9

Paint proofs and timing:
- waitForPaintedMarker(sid, marker, t=30s) — geometric paint proof — `smokeHarness.ts:388` — ~30F
- waitForPaintedCursor — `:323` — 1 (`terminal-paint-helpers.ts:66`)
- beginTerminalTiming — `:456` — ~8
- finishTerminalTiming — `:492` — ~8

Stream and transport probes:
- terminalBrowserSnapshot — `smokeTerminalStreamProbe.ts:25` — 5
- terminalStreamProbe — coord `diagSnapshot` + worker — `:28` — 3 (also `runFlow` catch)
- probeTerminalTransport — `:45` — 2
- phaseTimeline — `smokeRuntimeControls.ts:57` — 3
- retainedMarkerScan — `smokeRetainedMarkerScan.ts:18` — 8

Visibility and Sync controls (`smokeRuntimeControls.ts`):
- state() — `:60` — ~25 (fixtures, `terminal-delivery`, `upgrade/upgrade-probes.ts:243`)
- forceVisible(on) — `:130` — ~20 incl. `fixtures.ts:258,301`
- forceHidden(on) — `:~133` — 4
- forceSyncMaxBackoff — `:68` — 2
- syncRedialStatus — `:71` — 4
- pauseSyncTransport — `:74` — ~8
- resumeSyncTransport — `:77` — ~7
- syncWsGeneration — `:127` — ~15
- navigate(href) — `CustomEvent("roost-smoke-navigate")` — `:136-138` — ~8

Frame counters and fault injection (`smokeRuntimeControls.ts` unless noted):
- cellFrameCount — `:80` — ~5
- cellFullFrameCount — `:83` — ~10
- lastFullFrameSbRows — `:86` — ~12
- scrollbackBackfillRequestCount — `:89` — ~10
- directHistoryResponseCount — `smoke.ts:97` — 5
- cellGridEpoch — `:92` — 2
- blackholeTerminalFramesForCurrentGeneration — `:95` — 1
- dropNextTerminalWireDelta — `:98` — 1
- dropNextCellFrame — `:101` — 6
- droppedCellFrameCount — `:104` — 7
- holdTerminalDomForCurrentGeneration — `smokeTerminalDomFault.ts:86` — 1
- releaseTerminalDomHold — `:132` — 1
- perfProbe — `:107` — 3
- resetPerfCounters — `:124` — 1

Resources (`smokeCreatedResources.ts`) and stress:
- kill(sid) — `sessionsKill` — `:84` — 0 (only raw `process.kill` matches)
- spawnShell(fp, folder, sid?) — `sessionsSpawn{workerFp, kind:"shell", folder, sessionId}` + track — `:88` — ~28F
- createWorkspace(fp, folder, sid) — `workspacesCreate{…attachSessionIds}` + track — `:103` — 1 (`terminal-helpers.ts:86`) + `runFlow`
- trackCreatedSession — `:99` — 5
- cleanupCreated() — kill tracked sessions; delete tracked workspaces with `ifVersion`, 2 attempts — `:51` — 5 (`fixtures` ×2, peer/fast-path helpers, `worker-route-guards`)
- runFlow — `smokeHarness.ts:527` — 1 (`terminal-delivery.spec.ts:28`)
- runRenderStress — `:576` — 2 (`terminal-render.spec.ts:157,198`)

File transfer — 0 terminal-spec callers:
- uploadAttachment — `smokeFileTransferProbes.ts:16`
- attachmentProbe — `:24`
- downloadWorkerFile — `:34`

Created resources persist in `sessionStorage["roostSmoke.created.v1"]` across reloads (`smokeCreatedResources.ts:20-47,133`).

**runFlow step by step** (`smokeHarness.ts:527-574`; the U-1 gate is `terminal-delivery.spec.ts:15-31`, which calls `runFlow({workerFp: stack.workerFp})` and expects zero failing steps)
1. Read `state().workers`, sort by `last_seen_ms` desc, use `options.workerFp` or the first → record `worker_available` (`:535-540`).
2. `spawnShell(workerFp, "/tmp")` → Connect `SessionsSpawn{workerFp, kind:"shell", folder:"/tmp"}`; the session is tracked.
3. `history.pushState({}, "", "/s/<sid>")` + dispatch `popstate` (`:544-545`). The router must mount a terminal pane for `/s/:id` inside `[data-testid=terminal-slot-<sid>]` (`components/deck/TerminalDeck.tsx:93-94`).
4. Wait ≤300 rAF for `renderProbe(sid).found`, i.e. `slot .cell-grid` exists → record `shell_painted` (`:546-547`). Then wait ≤300 rAF for `nonEmptyRows>0` (not recorded) (`:548`).
5. `createWorkspace(workerFp, "/", sid)` → `workspacesCreate{name=basename or "~"(+suffix), folderPath:"/", attachSessionIds:[sid]}` → record `workspace_created` (`:550-551`).
6. `marker = ROOST_SMOKE_<8hex>`; `input(sid, "printf '%s\\n' <marker>\n")`, which awaits an accepted input outcome (`smokeTerminalInputController.ts:58-63`).
7. `waitForPaintedMarker(sid, marker)` (30s). Requirements (`:171-196`, `:388-445`):
   - slot and `.cell-grid` are connected with visible computed style;
   - the grid rect intersects the visual viewport;
   - some `.cell-row` contains the marker with a non-zero Range rect inside both grid and viewport;
   - the same row node and rects are stable after 2 rAF.
   → record `shell_round_trip`.
8. catch → `terminalStreamProbe(sid)` → record `flow_exception`. finally → `cleanupCreated()` → record `cleanup` (pass iff no errors) (`:557-570`).

RPCs used by runFlow: SessionsSpawn, WorkspacesCreate, (Sync input), SessionsKill, WorkspacesList, WorkspacesDelete; DiagSnapshot only on failure.

## E. TERMINAL MOUNT

**v2 `apps/web/src/components/terminal/*`** (lines — role)
- CellTerminal.tsx 346 — composes the pane: `createTerminalView`, controllers, overlays
- cell-terminal-renderer.ts 373 — mounts `CellGridRenderer`, backfill, prediction, input controller, stream feeds
- cell-terminal-presentation.ts 295 — readiness, reader holds, liveness, notices
- cell-terminal-lifecycle.ts 268 — viewport, resize, visibility → active/inactive view intent
- cell-terminal-viewport.ts 244 — cell measurement + debounced viewport (view) publication
- cell-terminal-interactions.ts 246 — links, selection, focus, drop, mouse forwarding
- cell-terminal-input.ts 223 — paste, clipboard, find, modifier input paths
- cell-terminal-dom-repair.ts 187 — DOM reconcile proof deadline → view redial
- cell-terminal-document-lifecycle.ts 92 — shared page-lifecycle fan-out
- cell-terminal-runtime.ts 54 — per-pane imperative resource holder
- cell-terminal-types.ts 21 — props
- terminalCaptureMenuController.ts 191, TerminalCaptureConsentDialog.tsx 66, TerminalCaptureStateRow.tsx 61 — incident capture
- TerminalContextMenu.tsx 394, TerminalSheetItem.tsx 50 — context menu
- TerminalComposeButton.tsx 429, TerminalComposeDictation.ts 112, TerminalComposePaneGeometry.ts 116 — composer
- TerminalFindBar.tsx 120 — find
- TerminalNavButtons.tsx 135 — touch key sheet
- TerminalOfflineNotice.tsx 81 — no-frame notice
- TerminalStartupOverlay.tsx 282 + .css 122 — opening card
- TerminalTransportIndicator.tsx 30 — carrier label
- TerminalCard.tsx 199 — mobile grid card

**Minimal set runFlow + waitForPaintedMarker depend on** [INFERENCE from the DOM/API paths used]
- Slot: `deck/TerminalDeck.tsx:93-94`.
- Pane: CellTerminal + cell-terminal-renderer + runtime + types + viewport + lifecycle + presentation (a visible grid is required by `hasVisibleComputedStyle`).
- Renderer: `renderer/cellRenderer.ts` + `cellRendererDom.ts:41-53`. Container classes are `wterm cell-grid`; children are `.cell-sb-spacer`, `.cell-scrollback`, `.cell-viewport`, `.cell-cursor`; rows are `.cell-row`.
- View: `store/terminal-stream.ts` (`createTerminalView`).
- Input: `store/transport/sync-outbound.ts:311`. `input()` bypasses the textarea, so interactions, input, composer, find and menus are NOT needed for U-1.

**Rust `crates/roost-web-terminal/src`** (`lib.rs:19-36`)
- Modules: backfill{request/wave, direct_history}, block_placeholder, cell_geometry, cell_renderer{eviction, history_page, ingest, paint, probe, reader, reconcile, scroll_events, scrollback}, cell_renderer_dom, cell_row{dom}, echo_overlay{geometry}, element_style, find{controller/chain, host, hits, intent}, input{dom, compose_selection/deferrals, selection, chord, keys}, link_target, links{dom, scan, activation}, mouse_forward{report, forwarding}, painted_history, presentation, reader_intent, scheduler{frames, frame_gate, cursor_poll}.
- `CellGridRenderer` (`cell_renderer.rs:53`, `#[wasm_bindgen]`):
  - `new(&Element)` (`:185-187`) or `with_callbacks(container, on_first_reconcile, on_reconcile, request_follow_band_settle)` (`:191-…`).
  - The container must already be in the document. The renderer appends `.cell-sb-spacer`, `.cell-scrollback`, `.cell-viewport` and stamps `wterm` + `cell-grid` (`:47-52`).
  - Feeding: `apply(&roost_protocol::cell::CellGridFrame)` → full or delta (`cell_renderer/ingest.rs:18`), `apply_full_frame` (`:32`), `apply_delta_frames(&[..])` (`:80`).
  - Accessors: `container()`, `prediction_host()` (`.cell-viewport`), `cursor_element()`, `current_frame()` (`cell_renderer.rs` ~`:252-280`).
  - Paint extras: `set_predicted_cursor` / `set_find_highlights` (`paint.rs:142,205`).
  - Frame batching via `scheduler::RenderScheduler` (`scheduler.rs:83-312`: `enqueue(frame, canonical)`, `schedule_browser_frame`, `on_frame_fired(now, hold_mask)`, `complete_paint`).
- The renderer is not referenced by any roost-web src file. The Cargo description promises "a session-keyed registry" (`crates/roost-web/Cargo.toml:8`), but none exists, and no `/s/:id` surface is served (`app.rs:73-90`: `Route::Session` is `NotServed`).

**Per-session grid state in roost-client-core**
- `Store.terminal: BTreeMap<String, TerminalSession>` (`store.rs:89`).
  - Created by `terminal_mut(sid, fp)` on ViewOpened (`store.rs:227-231`, `handle_terminal.rs:54`).
  - Read by `terminal(sid)` (`:234`).
- `TerminalSession` (`terminal/session.rs:64`):
  - `canonical() -> Option<&CellGridFrame>` (`:117`)
  - `baseline_ready()` (`:123`)
  - `frame_revision()` (`:132`): count of applied frames
  - `bind_generation` (`:163`)
  - `admit_frame` / `admit_chunk` (`:207,274`)
- The host watches `store.revision()` (`store.rs:244`). A cell frame bumps both `frame_revision` and the store revision (`apply_frame.rs:127-131`). The host then compares per-session `frame_revision` and feeds `canonical()` into `CellGridRenderer::apply` [INFERENCE: both use the `roost_protocol::cell::CellGridFrame` type].