// The UI state bounds, and the one place they are declared.
//
// v2 declares these in `packages/protocol/src/ui-state.ts:9` and BOTH the
// coordinator and the web client read them. A copy inside `roost-coord` would
// be a second answer to "how many tabs may one fingerprint hold", and the web
// client validating against a different number than the coordinator enforces is
// exactly the split the other bounds in this crate exist to prevent.

pub const UI_TAB_ID_MAX_UTF8_BYTES: usize = 256;
pub const UI_ACTIVE_PATH_MAX_UTF8_BYTES: usize = 8_192;
pub const UI_FOLDER_KEY_MAX_UTF8_BYTES: usize = 8_192;
pub const UI_STATE_MAX_TABS_PER_FINGERPRINT: usize = 32;
pub const UI_STATE_MAX_TABS_TOTAL: usize = 256;
pub const UI_STATE_NEW_IDENTITIES_PER_WINDOW: usize = 16;
pub const UI_STATE_IDENTITY_WINDOW_MS: i64 = 60_000;
