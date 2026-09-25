//! The coordinator service: SQLite state, auth, Connect RPC handlers, the Sync and worker WebSocket links, terminal hubs, and web push. Speaks the wire, never the terminal.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]
