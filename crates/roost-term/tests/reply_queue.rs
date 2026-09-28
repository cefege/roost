//! The core's reply queue: what `write_raw` provokes, `get_response` pops one
//! reply per call, oldest first, and `None` once drained. The worker's
//! query-reply lane (`crates/roost-worker/src/session/query_reply.rs`) writes
//! each drained reply straight onto the application's stdin, so an order the
//! core did not produce is a cursor report answering a status query. Ports
//! the response-queue half of v2 `packages/wterm/tests/wterm-core-load.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{AlacrittyCore, TerminalCore};

#[test]
fn replies_pop_one_per_call_in_the_order_the_probes_arrived() {
    let mut core = AlacrittyCore::new(80, 24);
    core.write_raw(b"\x1b[6n\x1b[5;7H\x1b[6n\x1b[5n");

    assert_eq!(core.get_response().as_deref(), Some("\x1b[1;1R"));
    assert_eq!(core.get_response().as_deref(), Some("\x1b[5;7R"));
    assert_eq!(core.get_response().as_deref(), Some("\x1b[0n"));
    assert_eq!(core.get_response(), None, "a drained queue answers nothing");
}
