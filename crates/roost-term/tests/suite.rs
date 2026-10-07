//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "cell_model.rs"]
mod cell_model;
#[path = "clipboard_writes.rs"]
mod clipboard_writes;
#[path = "dyn_dispatch_parity.rs"]
mod dyn_dispatch_parity;
#[path = "emitter_row_cap.rs"]
mod emitter_row_cap;
#[path = "kitty_keyboard.rs"]
mod kitty_keyboard;
#[path = "prompt_marks.rs"]
mod prompt_marks;
#[path = "reply_queue.rs"]
mod reply_queue;
#[path = "span_encoder.rs"]
mod span_encoder;
#[path = "terminal_core_vectors.rs"]
mod terminal_core_vectors;
#[path = "unhandled_csi_parity.rs"]
mod unhandled_csi_parity;
