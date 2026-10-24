//! The modal focus-scope rules the dialog family applies, as data: which
//! elements are tabbable, which way a focus-trap sentinel sends focus, and the
//! preventable auto-focus request a caller may cancel. Called by `dialog.rs`
//! and the DOM adapter in `dom.rs`; depends on nothing.

use std::cell::Cell;
use std::rc::Rc;

/// Everything that can take keyboard focus. `tabindex="-1"` and the trap's own
/// sentinels are excluded by the adapter after the query, because a selector
/// cannot compare a number.
pub const FOCUSABLE_SELECTOR: &str = "input:not([type='hidden']):not([disabled]), \
     select:not([disabled]), textarea:not([disabled]), button:not([disabled]), \
     a[href], area[href], [tabindex], iframe, object, embed, audio[controls], \
     video[controls], [contenteditable]:not([contenteditable='false'])";

/// The attribute that marks a focus-trap sentinel, so the tabbable query can
/// skip it.
pub const FOCUS_TRAP_ATTRIBUTE: &str = "data-focus-trap";

/// Which end of the content a sentinel hands focus to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEdge {
    /// The first tabbable element.
    First,
    /// The last tabbable element.
    Last,
}

/// Where focus goes when it lands on a sentinel.
///
/// Tabbing forward off the last element reaches the end sentinel and wraps to
/// the first; tabbing backward off the first reaches the start sentinel and must
/// wrap to the last. The two are told apart by where focus came from: leaving
/// the first element means focus is moving backwards.
pub fn sentinel_focus_edge(came_from_first: bool) -> FocusEdge {
    if came_from_first {
        FocusEdge::Last
    } else {
        FocusEdge::First
    }
}

/// Whether an element with this `tabindex` attribute is in the sequential tab
/// order. A missing or unparsable value is the element's natural order.
pub fn is_in_tab_order(tabindex: Option<&str>) -> bool {
    tabindex
        .and_then(|value| value.trim().parse::<i32>().ok())
        .is_none_or(|index| index >= 0)
}

/// A cancellable auto-focus: the dialog's default focus move happens unless a
/// caller's handler prevents it.
#[derive(Debug, Clone, Default)]
pub struct AutoFocusRequest {
    prevented: Rc<Cell<bool>>,
}

impl AutoFocusRequest {
    /// A request nobody has prevented.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take over focus placement; the dialog will not move focus.
    pub fn prevent_default(&self) {
        self.prevented.set(true);
    }

    /// Whether a handler took over focus placement.
    pub fn default_prevented(&self) -> bool {
        self.prevented.get()
    }
}

impl PartialEq for AutoFocusRequest {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.prevented, &other.prevented)
    }
}
