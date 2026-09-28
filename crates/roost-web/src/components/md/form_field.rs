//! The id and `aria-describedby` rules the form primitives share. Called by
//! `TextField`, `Select` and `SwitchRow`; depends on nothing.
//!
//! v2 built these inline in `apps/web/src/components/Settings/md/TextField.tsx`
//! and got the same order from Kobalte's form-control field for `Select.tsx`: the
//! caller's own ids first, then the visible description, then the error — so a
//! screen reader reads the caller's context, then the help, then what is wrong.

use dioxus::core::current_scope_id;

/// The ids an input's description reads from, space-joined, or `None` when
/// there are none — an empty `aria-describedby` points at nothing.
pub fn described_by(
    caller: Option<&str>,
    description_id: Option<&str>,
    error_id: Option<&str>,
) -> Option<String> {
    let ids: Vec<&str> = [caller, description_id, error_id]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    (!ids.is_empty()).then(|| ids.join(" "))
}

/// The id of a control's description element.
pub fn description_id(control_id: &str) -> String {
    format!("{control_id}-description")
}

/// The id of a control's error element.
pub fn error_id(control_id: &str) -> String {
    format!("{control_id}-error")
}

/// An id unique among the live components, for the component calling it.
///
/// v2 minted these with `crypto.randomUUID()` and `createUniqueId()`. The scope id
/// is unique for as long as the component is mounted, which is exactly how long
/// the `for`/`aria-*` references built from it are read.
pub fn scoped_element_id(prefix: &str) -> String {
    format!("{prefix}-{}", current_scope_id().0)
}
