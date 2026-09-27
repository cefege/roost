//! The event-core tests' shared support, by concern: what a test runs against is
//! `fixture`, the values it asserts on are `builders`, `reader` is the
//! synchronous second connection the recorder probes, and `reachability` holds
//! the two guards `event_reachability.rs` calls. This file is an index and
//! nothing else -- `fixture`, `builders` and `reader` are reached through it, so
//! those names have one path in. The two guards are the exception: they are
//! named at the point of use, because an index cannot re-export names only some
//! of its consumers call without warning in every binary that compiles it.

#![allow(dead_code)]
// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// `pub` so `event_reachability.rs` can name the two guards at the point of
// use. A shared index cannot re-export names only some of its consumers call
// without warning in every binary that compiles it, and event_append,
// event_query and event_publication all compile this index without calling
// these two.
pub mod reachability;

mod reader;

pub use reader::SyncReader;

mod fixture;

pub use fixture::*;

mod builders;

pub use builders::*;

