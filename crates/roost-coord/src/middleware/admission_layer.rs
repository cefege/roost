//! The Host/Origin admission layer mounted in front of both WebSocket routes.
//! Owned by the middleware slice; the module declaration lives in
//! `middleware/mod.rs` so the audit slice can be mounted underneath it without
//! either editing the other's file.
