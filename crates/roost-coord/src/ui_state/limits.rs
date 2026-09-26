// The browser UI ingress bounds, re-exported from their one owner.
//
// v2 declares these in `packages/protocol/src/ui-state.ts:9`, and BOTH the
// coordinator and the web client read them. They live in `roost-protocol` for
// the same reason every other wire-shaped bound in that crate does: the web
// client validates against the same numbers the coordinator enforces, and a
// copy here would be a second answer to "how many tabs may one fingerprint
// hold".
//
// This module exists only so `fence` and the two owners can say
// `ui_state::limits::UI_STATE_MAX_TABS_TOTAL` and not a bare constant name.

pub use roost_protocol::ui_state::*;
