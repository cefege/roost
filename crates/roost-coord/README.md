# roost-coord

The Roost v3 coordinator: the HTTP/Connect front door, the worker link
(`/ws/coord-worker/{fp}`), the browser Sync firehose (`/ws/coord-sync`), the
durable event log and every RPC domain. Each module's `//!` header names what it
owns, who calls it and what it depends on.

Entry point: `serve::serve` (called by `roost coord`). Composition root:
`services::CoordServices` (one runtime per domain) and `serve.rs` (boot order,
listeners, background owners). RPC routing: `rpc/method_route_rows.rs` (one row per
`CoordinatorService` method, with its owning domain and auth requirement) paired with
`rpc/service_impl.rs` (one arm per method).

## Deliberately absent

| Not here | Why |
|---|---|
| Adopting another product's database | The coordinator opens only its own database (sqlx migrations in `db.rs`); a database written by another product is never adopted in place. |
| Signed Windows update deploys | The coordinator starts POSIX `roost deploy` jobs only; Windows workers follow the coordinator through their own updater, and `deploy::update_progress` refuses progress for a signed Windows update aloud. |
| A reader of route retirements | Titles arrive only as compact terminal metadata frames, so no per-route parser state exists and route retirement has no reader: `terminal_screen/route_index.rs` keeps `NoRouteRetirement`. |
