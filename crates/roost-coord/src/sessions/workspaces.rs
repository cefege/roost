//! The workspace tree: its rows, its ordering, and the cascade a delete
//! performs. Owned by the workspaces slice; the module declaration lives in
//! `sessions/mod.rs` so the tasks and MCP slices do not touch it.
