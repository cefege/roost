//! The link attachment's host seam and event vocabulary: `LinkHost` (the
//! interaction operations beyond the scanner's), the `LinkListener` set in
//! v2's attach order, and the `LinkEvent` each dispatch reads. `links::dom`
//! implements the host and wires every listener into `TerminalLinks::dispatch`.
//! Ports the listener wiring of `attachTerminalLinks` in
//! `apps/web/src/renderer/terminal-links.ts`.

use crate::links::activation::LinkActivationGesture;
use crate::links::scan::LinkScanHost;

/// The interaction operations the attachment needs beyond the scanner's.
pub trait LinkHost: LinkScanHost {
    /// The terminal display element the attachment is attached to.
    fn container(&self) -> Self::Element;
    /// Attach one interaction listener (window or container, per `listener`).
    fn add_listener(&self, listener: LinkListener);
    /// Detach one interaction listener.
    fn remove_listener(&self, listener: LinkListener);
    /// Show the floating hint with `text` under `anchor`, creating it once.
    fn show_hint(&self, anchor: &Self::Element, text: &str);
    /// Hide the floating hint if it exists.
    fn hide_hint(&self);
    /// Remove the floating hint element for good.
    fn remove_hint(&self);
    /// Click `anchor` as a user would: appended to the body, clicked, removed.
    fn click_detached_anchor(&self, anchor: &Self::Element);
}

/// One interaction listener, in v2's attach order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkListener {
    /// window `keydown`.
    KeyDown,
    /// window `keyup`.
    KeyUp,
    /// window `blur`.
    Blur,
    /// container `mouseover`.
    MouseOver,
    /// container `mouseout`.
    MouseOut,
    /// container `mouseenter`.
    MouseEnter,
    /// container `mouseleave`.
    MouseLeave,
    /// container `mousemove`.
    MouseMove,
    /// container `mousedown`.
    MouseDown,
    /// container `click`.
    Click,
}

impl LinkListener {
    /// Every listener an active attachment holds.
    pub const ALL: [Self; 10] = [
        Self::KeyDown,
        Self::KeyUp,
        Self::Blur,
        Self::MouseOver,
        Self::MouseOut,
        Self::MouseEnter,
        Self::MouseLeave,
        Self::MouseMove,
        Self::MouseDown,
        Self::Click,
    ];

    /// The DOM event type.
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::KeyDown => "keydown",
            Self::KeyUp => "keyup",
            Self::Blur => "blur",
            Self::MouseOver => "mouseover",
            Self::MouseOut => "mouseout",
            Self::MouseEnter => "mouseenter",
            Self::MouseLeave => "mouseleave",
            Self::MouseMove => "mousemove",
            Self::MouseDown => "mousedown",
            Self::Click => "click",
        }
    }

    /// Whether it listens on the window rather than the container.
    pub const fn on_window(self) -> bool {
        matches!(self, Self::KeyDown | Self::KeyUp | Self::Blur)
    }
}

/// One DOM event as the attachment reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkEvent<E> {
    /// `KeyboardEvent.key`, for key events.
    pub key: Option<String>,
    /// Button and modifier levels; all false for an event carrying none.
    pub gesture: LinkActivationGesture,
    /// `closest("a.wterm-link")` of the event target, for over/out/click.
    pub anchor: Option<E>,
}
