//! The top banner that says the browser is offline or the coordinator is not
//! answering, with the Reconnect action. Its two sources are the browser's own
//! `navigator.onLine` and the client's live Sync link, read from the store
//! rather than from a health poller this build does not run.
//! Ports `apps/web/src/components/notifications/ConnectionBanner.tsx`, dropping
//! its `window.__roostCoordHealth` polling because the Rust client publishes no
//! such snapshot: the link generation IS the liveness signal, and a link that
//! closed carries its own redial in the core.

use dioxus::prelude::Signal;
use dioxus::prelude::*;

use crate::components::md::{Button, ButtonSize, ButtonVariant, StatusDot, Surface, SurfaceRadius};
use crate::components::terminal::dom::{now_ms, page_visible, sleep_ms};
use crate::pump::{Pump, use_store};

/// How often a tab re-reads its own liveness. The core redials on its own
/// schedule; this only decides whether the banner is still true.
const EVALUATE_INTERVAL_MS: u64 = 2_000;

/// A link that has been open this long without a frame is stale, not dead — the
/// core redials a silent socket, and a banner that fires before it has would
/// accuse a coordinator that is merely quiet.
const STALE_AFTER_MS: u64 = 10_000;

/// Why the banner is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerReason {
    /// The browser says it has no network.
    Offline,
    /// The Sync link is not open, or has gone quiet past its stale bound.
    CoordUnreachable,
    /// The coordinator rejected this browser's credential; no redial fixes it.
    AuthRevoked,
}

impl BannerReason {
    /// The `data-banner-reason` spelling v2's tests and the smoke specs select.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::CoordUnreachable => "coord-unreachable",
            Self::AuthRevoked => "coord-auth-revoked",
        }
    }

    /// The line the operator reads.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Offline => "Offline — check your network connection",
            Self::CoordUnreachable => "Coordinator unreachable — sessions paused",
            Self::AuthRevoked => "This browser's access was revoked — sign in again",
        }
    }

    /// Whether a Reconnect button would do anything. A revoked credential is
    /// not a stale socket; offering the button would be offering a no-op.
    pub const fn offers_reconnect(self) -> bool {
        matches!(self, Self::CoordUnreachable)
    }
}

/// The banner, or nothing when the client is healthy.
#[component]
pub fn ConnectionBanner() -> Element {
    let pump = use_store();
    let mut reason = use_signal(|| None::<BannerReason>);
    let quiet_since = use_signal(|| None::<u64>);

    // One loop over one source of truth: the tab's own visibility and the
    // store's link, re-read on a tick rather than from six separate listeners.
    use_future(move || {
        let pump = pump.clone();
        async move {
            loop {
                evaluate(&pump, reason, quiet_since);
                sleep_ms(EVALUATE_INTERVAL_MS).await;
            }
        }
    });

    let verdict = reason();
    let Some(verdict) = verdict else {
        return rsx! {};
    };
    rsx! {
        div {
            "data-testid": "connection-banner",
            "data-banner-reason": verdict.as_str(),
            style: "position: fixed; inset: 0 0 auto; z-index: 50;",
            Surface {
                level: 2,
                elevation: 2,
                radius: SurfaceRadius::Xs,
                style: "display: flex; align-items: center; justify-content: center; gap: var(--md-space-3); padding: var(--md-space-2) var(--md-space-4); border-bottom: var(--workbench-border-width) solid var(--md-sys-color-error); color: var(--md-sys-color-on-surface); font: var(--md-body-s-weight) var(--md-body-s-size)/var(--md-body-s-line) var(--md-font);".to_owned(),
                StatusDot { status: "error".to_owned() }
                span { "{verdict.message()}" }
                if verdict.offers_reconnect() {
                    Button {
                        variant: ButtonVariant::Secondary,
                        size: ButtonSize::Sm,
                        "data-testid": "connection-banner-reconnect",
                        onclick: move |_| reason.set(None),
                        "Reconnect"
                    }
                }
            }
        }
    }
}

/// Read the browser and the store once, and record why the banner is or is not
/// up. A hidden tab is not evaluated at all: the core skips a backgrounded
/// socket's bookkeeping, so a hidden tab's silence is not evidence of an outage.
fn evaluate(
    pump: &Pump,
    mut reason: Signal<Option<BannerReason>>,
    quiet_since: Signal<Option<u64>>,
) -> bool {
    if !page_visible() {
        return false;
    }
    let observed = observe(pump, quiet_since);
    reason.set(observed);
    observed.is_some()
}

/// The banner's verdict from the two live sources, and the bookkeeping that
/// turns an open-but-quiet link into a stale one exactly once.
fn observe(pump: &Pump, mut quiet_since: Signal<Option<u64>>) -> Option<BannerReason> {
    let now = now_ms();
    let (revoked, linked) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        (
            store.sync.auth_revoked,
            store.sync.link_generation().is_some(),
        )
    };
    if revoked {
        return Some(BannerReason::AuthRevoked);
    }
    if !crate::platform::network::browser_online() {
        return Some(BannerReason::Offline);
    }
    if !linked {
        quiet_since.set(None);
        return Some(BannerReason::CoordUnreachable);
    }
    let since = quiet_since().unwrap_or(now);
    let verdict =
        (now.saturating_sub(since) > STALE_AFTER_MS).then_some(BannerReason::CoordUnreachable);
    quiet_since.set(Some(if verdict.is_some() { since } else { now }));
    verdict
}
