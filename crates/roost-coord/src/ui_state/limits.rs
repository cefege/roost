// The browser UI ingress bounds, re-exported from their one owner.
//
// The coordinator and the web client both read these bounds. They live in
// `roost-protocol` for the same reason every other wire-shaped bound in that
// crate does: the web client validates against the same numbers the
// coordinator enforces, and a copy here would be a second answer to "how many
// tabs may one fingerprint hold".
//
// This module exists only so `fence` and the two owners can say
// `ui_state::limits::UI_STATE_MAX_TABS_TOTAL` and not a bare constant name.

pub use roost_protocol::ui_state::*;
