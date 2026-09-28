//! The page-lifecycle edges one terminal pane reacts to, and what each one
//! does to its view: a hidden page parks it, `pagehide` withdraws it, and a
//! page that shows again republishes an active pane. Target-independent; the
//! wasm pane mount listens and applies the action. Ports
//! `apps/web/src/components/terminal/cell-terminal-document-lifecycle.ts` and
//! the lifecycle callback of `cell-terminal-lifecycle.ts`.

/// One page-lifecycle edge, already resolved against page visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentLifecycleEvent {
    /// The page became hidden (or showed while hidden).
    Hidden,
    /// The page is being unloaded or frozen into the back/forward cache.
    PageHide,
    /// The page became visible.
    Visible,
    /// The page was restored from the back/forward cache while visible.
    PageShow,
    /// A frozen page resumed while visible.
    Resume,
}

/// The DOM event types the pane listens for, with the target each lives on.
pub const DOCUMENT_LIFECYCLE_EVENTS: [(&str, LifecycleTarget); 4] = [
    ("visibilitychange", LifecycleTarget::Document),
    ("resume", LifecycleTarget::Document),
    ("pagehide", LifecycleTarget::Window),
    ("pageshow", LifecycleTarget::Window),
];

/// Where a lifecycle listener is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleTarget {
    Document,
    Window,
}

/// Resolve one DOM event against the page's visibility at delivery time.
pub fn classify(event_type: &str, page_visible: bool) -> Option<DocumentLifecycleEvent> {
    let shown = |visible_edge| {
        if page_visible {
            visible_edge
        } else {
            DocumentLifecycleEvent::Hidden
        }
    };
    match event_type {
        "visibilitychange" => Some(shown(DocumentLifecycleEvent::Visible)),
        "pagehide" => Some(DocumentLifecycleEvent::PageHide),
        "pageshow" => Some(shown(DocumentLifecycleEvent::PageShow)),
        "resume" => Some(shown(DocumentLifecycleEvent::Resume)),
        _ => None,
    }
}

/// What the pane does on one edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    /// Release paint holds, clear activity and blink, and withdraw the view.
    Park,
    /// Withdraw, graced when the withdraw is only a layout gap.
    Withdraw,
    /// Re-decide presentation, republish the view now, and refresh the lease.
    Republish,
    /// Nothing to do.
    Ignore,
}

/// The action for one edge on a pane whose view is or is not active.
pub fn lifecycle_action(
    event: DocumentLifecycleEvent,
    page_visible: bool,
    view_active: bool,
) -> LifecycleAction {
    match event {
        DocumentLifecycleEvent::Hidden | DocumentLifecycleEvent::PageHide => LifecycleAction::Park,
        _ if !page_visible || !view_active => {
            if event == DocumentLifecycleEvent::Visible {
                LifecycleAction::Withdraw
            } else {
                LifecycleAction::Ignore
            }
        }
        DocumentLifecycleEvent::Visible
        | DocumentLifecycleEvent::PageShow
        | DocumentLifecycleEvent::Resume => LifecycleAction::Republish,
    }
}
