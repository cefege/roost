//! Owns the toast queue: entries, their enter/exit/auto-dismiss timers, and the
//! handle callers push through. `toast.rs` renders this state; nothing here
//! touches markup.
//! Called from `toast.rs` (`ToastToaster`, `ToastTrigger`, `ToastItem`); depends
//! on `terminal::dom::sleep_ms` for the timing rust-ui's JS got from rAF and
//! transitionend.

use dioxus::prelude::*;

use super::toast::{ToastPosition, ToastType};
use crate::components::terminal::dom::sleep_ms;

/// rust-ui's `--dismiss-delay` default.
pub(crate) const DEFAULT_DISMISS_MS: u64 = 5000;
/// rust-ui's `--enter-duration` and `--exit-duration`.
const ENTER_MS: u64 = 300;
const EXIT_MS: u64 = 300;
/// Two animation frames in the JS bridge; a single short sleep here.
const MOUNT_MS: u64 = 16;
/// Granularity of the duration progress bar and of hover-pause checks.
const TICK_MS: u64 = 50;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToastEntry {
    pub id: u64,
    pub variant: ToastType,
    pub title: String,
    pub description: Option<String>,
    pub position: ToastPosition,
    pub mounted: bool,
    pub entering: bool,
    pub removed: bool,
    /// Remaining fraction of the auto-dismiss time, `1.0` down to `0.0`.
    pub progress: f64,
}

impl ToastEntry {
    /// Loading toasts stay until replaced; the JS bridge set no timer for them.
    fn is_timed(&self) -> bool {
        self.variant != ToastType::Loading
    }
}

/// Queue state provided by `Toaster`. Signals are created in the provider's
/// scope, so the hub is `Copy` and can move into spawned tasks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ToastHub {
    pub entries: Signal<Vec<ToastEntry>>,
    pub next_id: Signal<u64>,
    /// True while the pointer is over a toast list: timers pause and the list
    /// reports `data-expanded`.
    pub hovered: Signal<bool>,
}

impl ToastHub {
    /// Creates the queue signals; call only from a component body.
    pub(crate) fn use_new() -> Self {
        Self {
            entries: use_signal(Vec::new),
            next_id: use_signal(|| 0),
            hovered: use_signal(|| false),
        }
    }

    pub(crate) fn push(
        self,
        variant: ToastType,
        title: String,
        description: Option<String>,
        position: ToastPosition,
    ) {
        let mut next_id = self.next_id;
        let id = {
            let mut counter = next_id.write();
            *counter += 1;
            *counter
        };
        let entry = ToastEntry {
            id,
            variant,
            title,
            description,
            position,
            mounted: false,
            entering: true,
            removed: false,
            progress: 1.0,
        };
        let timed = entry.is_timed();
        let mut entries = self.entries;
        entries.write().push(entry);
        let _ = spawn(run_toast_lifecycle(self, id, timed));
    }

    pub(crate) fn dismiss(self, id: u64) {
        let _ = spawn(dismiss_toast(self, id));
    }

    fn update(self, id: u64, change: impl FnOnce(&mut ToastEntry)) {
        let mut entries = self.entries;
        if let Some(entry) = entries.write().iter_mut().find(|entry| entry.id == id) {
            change(entry);
        }
    }
}

/// Mount, enter, then count down the auto-dismiss, pausing while hovered.
async fn run_toast_lifecycle(hub: ToastHub, id: u64, timed: bool) {
    sleep_ms(MOUNT_MS).await;
    hub.update(id, |entry| entry.mounted = true);
    sleep_ms(ENTER_MS).await;
    hub.update(id, |entry| entry.entering = false);
    if !timed {
        return;
    }
    let mut remaining_ms = DEFAULT_DISMISS_MS;
    while remaining_ms > 0 {
        sleep_ms(TICK_MS).await;
        if *hub.hovered.peek() {
            continue;
        }
        remaining_ms = remaining_ms.saturating_sub(TICK_MS);
        let fraction = remaining_ms as f64 / DEFAULT_DISMISS_MS as f64;
        hub.update(id, |entry| entry.progress = fraction);
    }
    dismiss_toast(hub, id).await;
}

/// Marks the toast removed so it animates out, then drops it from the queue.
async fn dismiss_toast(hub: ToastHub, id: u64) {
    let mut entries = hub.entries;
    let already_removed = entries
        .read()
        .iter()
        .find(|entry| entry.id == id)
        .is_none_or(|entry| entry.removed);
    if already_removed {
        return;
    }
    hub.update(id, |entry| entry.removed = true);
    sleep_ms(EXIT_MS).await;
    entries.write().retain(|entry| entry.id != id);
}

/// Handle for pushing toasts from anywhere under `Toaster`.
#[derive(Clone, Copy, Debug)]
pub struct ToastHandle {
    hub: ToastHub,
}

impl ToastHandle {
    /// Shows a toast at `position`. `description` is optional body text.
    pub fn show(
        self,
        variant: ToastType,
        title: impl Into<String>,
        description: Option<String>,
        position: ToastPosition,
    ) {
        self.hub.push(variant, title.into(), description, position);
    }

    /// Starts the exit animation for toast `id` (the close button).
    pub fn dismiss(self, id: u64) {
        self.hub.dismiss(id);
    }
}

/// The handle for the nearest `Toaster`. Panics if no `Toaster` is above the caller.
pub fn use_toast() -> ToastHandle {
    ToastHandle {
        hub: use_context::<ToastHub>(),
    }
}
