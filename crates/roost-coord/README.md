# roost-coord

The Roost v3 coordinator: the HTTP/Connect front door, the worker link
(`/ws/coord-worker/{fp}`), the browser Sync firehose (`/ws/coord-sync`), the
durable event log and every RPC domain. It ports v2's `apps/coord/src`; each Rust
module's `//!` header names the v2 file(s) it ports.

Entry point: `serve::serve` (called by `roost coord`). Composition root:
`services::CoordServices` (one runtime per domain) and `serve.rs` (boot order,
listeners, background owners). RPC routing: `rpc/method_route_rows.rs` (one row per
`CoordinatorService` method, with its auth requirement and port status) paired with
`rpc/service_impl.rs` (one arm per method).

## Not ported, by decision

Every v2 `apps/coord/src` non-test module is either ported (a Rust `//!` header names
it) or listed here with the reason.

| v2 | Decision |
|---|---|
| `db/migrate.ts` — adoption of v2's `_migrations` table (`:296-354`) | v3 opens only its own database (sqlx migrations in `db.rs`); a v2 database is brought over once by `roost import-v2`, never adopted in place. |
| `deploy/windows-update-deploy-jobs.ts`, `deploy/windows-update-deploy-record.ts`, `deploy/windows-update-deploy-runtime.ts`, `deploy/windows-update-manifest.ts`, the Windows half of `workers/worker-send-maintenance.ts`, and `main.ts`'s `win32` `serveServiceHealth` branch (`:219-233`) | Windows is PAUSED for v0.5.0 (`FEATURES/README.md`); the POSIX paths are ported. |
| `terminal/input/terminal-route-retirement.ts` publication's only subscriber, `terminal/terminal-metadata-adapter.ts`'s legacy parser (`removeLegacyParser`, `:88`) | The coordinator refuses legacy `WBinary` terminal metadata (every v3 peer negotiates `terminal_metadata_v1`), so the legacy parser and its per-route state are not ported and route retirement has no reader: `terminal_screen/route_index.rs` keeps `NoRouteRetirement`. |
