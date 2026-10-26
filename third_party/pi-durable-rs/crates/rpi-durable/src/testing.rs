//! Runner-independent conformance suites exported for backend tests
//! (port of `src/testing/`, V17-13 FR-F).

pub mod storage_conformance;

pub use storage_conformance::{
    ConformanceCase, ConformanceFuture, create_storage_conformance, run_storage_conformance,
};
