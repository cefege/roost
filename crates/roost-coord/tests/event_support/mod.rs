//! The event-core tests' shared support, by concern: what a test runs against is
//! `fixture`, the values it asserts on are `builders`, `reader` is the
//! synchronous second connection the recorder probes, and `reachability` holds
//! the two guards `event_reachability.rs` calls. This file is an index and
//! nothing else -- both halves import through it, so a name has one path in.

#![allow(dead_code)]
// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod reachability;

mod reader;

pub use reader::SyncReader;

mod fixture;

pub use fixture::*;

mod builders;

pub use builders::*;

// Named rather than a glob, and the `unused_imports` warning this raises in the
// binaries that do not call them is the price: a glob re-export nothing imports
// two staying reachable under their own names. `event_reachability.rs` calls
// both; `event_publication.rs`, `event_append.rs` and `event_query.rs` never
// did, and stopped when the guard moved out of the publication binary.
pub use reachability::{
    the_deferred_append_path_has_an_execution_path, the_deferred_reap_ids_have_a_production_reader,
};
