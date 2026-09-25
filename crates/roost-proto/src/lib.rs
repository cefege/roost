//! Generated protobuf messages and Connect service stubs for the Roost wire
//! contract. `protocol/proto/roost/v1/*.proto` is the only source of truth;
//! build.rs compiles it with `connectrpc-build` and this module mounts the
//! result, so no Rust type in the workspace hand-writes a wire message.
//!
//! Depends on nothing: buffa and connectrpc are its whole world. Everything
//! that speaks the wire depends on this crate, never the other way round.

// This crate is one `include!` and nothing else, so every lint it trips comes
// from generated code this repository does not write:
//   - `elided_lifetimes_in_paths`, which the workspace's `rust_2018_idioms`
//     group turns on, fires 267 times in buffa's view types.
//   - `missing_debug_implementations` fires on the zero-copy view wrappers,
//     which are deliberately not `Debug` because materialising one defeats
//     their purpose.
//   - `clippy::len_without_is_empty` fires on generated `len()` accessors for
//     repeated fields whose emptiness is a proto3 default nobody queries.
// The generated file's own `#[allow]` list names none of them, and
// `cargo clippy -D warnings` would be unsatisfiable here without this.
#![allow(
    elided_lifetimes_in_paths,
    missing_debug_implementations,
    clippy::len_without_is_empty
)]

connectrpc::include_generated!();

// The runtime the generated code is written against. Re-exported so a crate
// that depends on roost-proto can encode and decode without taking a second
// protobuf dependency of its own — `roost-protocol`'s DAG allowlist names
// roost-proto, not buffa, and that is the shape this re-export preserves.
pub use buffa;

pub use roost::v1::*;

/// The protobuf package every message and service in the contract lives in.
pub const PROTO_PACKAGE: &str = "roost.v1";
