//! The Connect RPC surface: the method route table, and the ONE implementation
//! of `CoordinatorService`.
//!
//! Owned by the coordinator. `method_route` is the coverage guard that replaced
//! v2's "a second `router.service()` call shadows the rest" hazard, and
//! `service_impl` is the single `impl` block — one file, because Rust does not
//! permit one trait's impl to be split across blocks even with disjoint methods.
//!
//! The size of `service_impl` is a recorded exception rather than a split, and
//! the reason is in that file's header: splitting would be impossible anyway, and
//! the alternative — a macro generating 103 delegations — would hide 103 method
//! names behind a list, which is exactly what the no-metaprogramming rule exists
//! to prevent.

pub mod method_route;
pub mod method_route_rows;
pub mod service;
pub mod service_impl;
