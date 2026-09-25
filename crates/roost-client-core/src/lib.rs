//! The UI-free client: Connect client, Sync state machine, store fold, terminal-stream replica and its route election, input lanes, encoders, and find paging. Platform services arrive through traits so one state machine drives wasm, tokio and mobile hosts.
//! Owned here until a phase fills it in; nothing calls it yet.

#![forbid(unsafe_code)]
