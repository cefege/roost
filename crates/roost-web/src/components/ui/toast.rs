//! The generic in-page toast queue: `Toaster` provides a `ToastHandle` through
//! context to its `children` and draws the fixed region beside them, so the
//! whole app mounts as `Toaster { App {} }`; any descendant calls `use_toast()`
//! to push or dismiss. Ports rust-ui's `toast.rs`, with its JS trigger bridge
//! replaced by this signal-owned queue. Not the notification dock:
//! `notifications/toast_stack` renders the client store's session toasts, and
//! this queue reads nothing from it. Auto-dismiss timers use
//! `terminal::dom::sleep_ms`; native builds never fire them.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::terminal::dom::sleep_ms;
use crate::components::ui::{Icon, IconType};

/// Lifetime of a toast whose spec names no duration.
const DEFAULT_TOAST_DURATION_MS: u64 = 5_000;

/// Colour role of a toast's border; `Default` is the neutral card edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastVariant {
    #[default]
    Default,
    Success,
    Error,
    Warning,
    Info,
}

impl ToastVariant {
    const fn border_class(self) -> &'static str {
        match self {
            Self::Default => "border-border",
            Self::Success => "border-success",
            Self::Error => "border-destructive",
            Self::Warning => "border-warning",
            Self::Info => "border-info",
        }
    }
}

/// The corner or edge the toast region is pinned to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastPosition {
    TopLeft,
    TopCenter,
    TopRight,
    #[default]
    BottomRight,
    BottomCenter,
    BottomLeft,
}

impl ToastPosition {
    const fn region_class(self) -> &'static str {
        match self {
            Self::TopLeft => "left-6 top-6",
            Self::TopCenter => "left-1/2 -translate-x-1/2 top-6",
            Self::TopRight => "right-6 top-6",
            Self::BottomCenter => "left-1/2 -translate-x-1/2 bottom-6",
            Self::BottomLeft => "left-6 bottom-6",
            Self::BottomRight => "right-6 bottom-6",
        }
    }
}

/// What a caller asks for. Build with `ToastSpec::new(title)` and the setters.
#[derive(Clone, Debug, PartialEq)]
pub struct ToastSpec {
    pub title: String,
    pub description: Option<String>,
    pub variant: ToastVariant,
    /// `None` keeps the toast until its dismiss button is pressed.
    pub duration_ms: Option<u64>,
    /// Set on the toast row only when present.
    pub test_id: Option<String>,
}

impl ToastSpec {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            variant: ToastVariant::Default,
            duration_ms: Some(DEFAULT_TOAST_DURATION_MS),
            test_id: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn variant(mut self, variant: ToastVariant) -> Self {
        self.variant = variant;
        self
    }

    #[must_use]
    pub fn duration_ms(mut self, duration_ms: Option<u64>) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    #[must_use]
    pub fn test_id(mut self, test_id: impl Into<String>) -> Self {
        self.test_id = Some(test_id.into());
        self
    }
}

/// Identity of one toast, unique within the `Toaster` that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToastId(u64);

/// A queued toast: its identity plus the spec it was pushed with.
#[derive(Clone, Debug, PartialEq)]
struct ToastEntry {
    id: ToastId,
    spec: ToastSpec,
}

/// The push and dismiss API a `Toaster` provides. Holds only signals, so it is
/// `Copy` and can be captured by event handlers and timer tasks freely.
#[derive(Clone, Copy, Debug)]
pub struct ToastHandle {
    toasts: Signal<Vec<ToastEntry>>,
    next_id: Signal<u64>,
}

impl ToastHandle {
    /// Queue a toast and return its id so the caller can dismiss it early.
    pub fn push(&self, spec: ToastSpec) -> ToastId {
        let mut next_id = self.next_id;
        let id = {
            let mut counter = next_id.write();
            let id = ToastId(*counter);
            *counter += 1;
            id
        };
        tracing::debug!(
            target: "ui",
            toast_id = id.0,
            variant = ?spec.variant,
            timed = spec.duration_ms.is_some(),
            "toast pushed"
        );
        let mut toasts = self.toasts;
        toasts.write().push(ToastEntry { id, spec });
        id
    }

    /// Remove a toast by id. A toast that already timed out is a no-op.
    pub fn dismiss(&self, id: ToastId) {
        let mut toasts = self.toasts;
        let removed = {
            let mut queue = toasts.write();
            let before = queue.len();
            queue.retain(|entry| entry.id != id);
            queue.len() != before
        };
        if removed {
            tracing::debug!(target: "ui", toast_id = id.0, "toast dismissed");
        }
    }
}

/// The handle the nearest `Toaster` provides. Panics only when no `Toaster` is
/// mounted above the caller, which is a wiring error.
pub fn use_toast() -> ToastHandle {
    use_context::<ToastHandle>()
}

/// One toast row. It owns its auto-dismiss timer: the timer task lives in this
/// component's scope, so dismissing the toast by hand drops the task with it.
#[component]
fn ToastItem(entry: ToastEntry) -> Element {
    let handle = use_toast();
    let ToastEntry { id, spec } = entry;
    let ToastSpec {
        title,
        description,
        variant,
        duration_ms,
        test_id,
    } = spec;
    use_effect(move || {
        if let Some(lifetime_ms) = duration_ms {
            spawn(async move {
                sleep_ms(lifetime_ms).await;
                handle.dismiss(id);
            });
        }
    });
    let row_class = tw_merge!(
        "pointer-events-auto bg-background border rounded-md shadow-lg p-4 flex gap-2 w-full items-start",
        variant.border_class()
    );
    rsx! {
        li {
            class: "{row_class}",
            "data-testid": test_id,
            div { class: "grid flex-1 min-w-0 gap-1",
                p { class: "text-sm font-medium text-foreground", "{title}" }
                if let Some(text) = description {
                    p { class: "text-sm text-muted-foreground", "{text}" }
                }
            }
            button {
                r#type: "button",
                class: "inline-flex shrink-0 items-center justify-center rounded-md size-6 text-muted-foreground hover:text-foreground cursor-pointer outline-none focus-visible:ring-ring/50 focus-visible:ring-[3px]",
                "aria-label": "Dismiss notification",
                onclick: move |_| handle.dismiss(id),
                Icon { icon: IconType::X, class: "size-4" }
            }
        }
    }
}

/// The fixed live region that renders the queue, oldest first, and provides
/// `ToastHandle` to `children`. Wrap the app once: `Toaster { App {} }`. The
/// region is pointer-transparent; each toast row opts back in, so the gaps
/// between rows never block the page.
#[component]
pub fn Toaster(
    children: Element,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] label: Option<String>,
    #[props(default = ToastPosition::BottomRight)] position: ToastPosition,
) -> Element {
    let toasts = use_signal(Vec::<ToastEntry>::new);
    let next_id = use_signal(|| 0_u64);
    use_context_provider(|| ToastHandle { toasts, next_id });
    let merged_class = tw_merge!(
        "fixed z-50 w-96 max-w-full pointer-events-none",
        position.region_class(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        {children}
        div {
            class: "{merged_class}",
            role: "region",
            "aria-label": label.as_deref().unwrap_or("Toasts"),
            "aria-live": "polite",
            ol { class: "flex flex-col gap-4",
                for entry in toasts() {
                    ToastItem { key: "{entry.id.0}", entry }
                }
            }
        }
    }
}
