//! Consumed subset of `@earendil-works/chord` (port of `packages/chord` @
//! abe508e1b).
//!
//! Only the surfaces durable consumes are carried (V17-13 FR-B): the cancellation
//! context family, `copyJson`/`JsonValue` helpers, the `Op` delta applier, and the
//! `replicatedState` document-state service. Bundler/facets/node and protocol stay
//! [DEFER] (R5).

pub mod context;
pub mod delta;
pub mod json;
pub mod state;
pub mod tracker;
