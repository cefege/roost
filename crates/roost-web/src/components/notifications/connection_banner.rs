//! The top banner that says the browser is offline or the coordinator is not
//! answering, with the Reconnect action. Its sources are the browser's own
//! `navigator.onLine` and the client's live Sync link, judged by the core's
//! `sync_link_answering` — the predicate the status bar reads too — so the
//! banner keeps no clock of its own and clears as soon as the link answers.
//! Ports `apps/web/src/components/notifications/ConnectionBanner.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::terminal_transport::has_liveness_qualified_direct_terminal;
use roost_client_core::sync::redial::sync_link_answering;
use roost_client_core::{ClientEvent, TransportControl};

use crate::components::md::{Button, ButtonSize, ButtonVariant, StatusDot, Surface, SurfaceRadius};
use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::pump::{Pump, use_store};

/// How often a visible tab re-reads its own liveness. The core redials on its
/// own schedule; this only decides whether the banner is still true.
const EVALUATE_INTERVAL_MS: u64 = 2_000;

/// Why the banner is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerReason {
    /// The browser says it has no network.
    Offline,
    /// The Sync link is not open, or has gone quiet past its stale bound.
    CoordUnreachable,
    /// The same outage while a direct terminal still holds a current proof: a
    /// partial degradation, because that terminal continues without the
    /// coordinator while fleet controls cannot.
    CoordUnreachableDirectLive,
    /// The coordinator rejected this browser's credential; no redial fixes it.
    AuthRevoked,
}

impl BannerReason {
    /// The `data-banner-reason` spelling v2's tests and the smoke specs select.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::CoordUnreachable => "coord-unreachable",
            Self::CoordUnreachableDirectLive => "coord-unreachable-direct-live",
            Self::AuthRevoked => "coord-auth-revoked",
        }
    }

    /// The line the operator reads.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Offline => "Offline — check your network connection",
            Self::CoordUnreachable => "Coordinator unreachable — sessions paused",
            Self::CoordUnreachableDirectLive => {
                "Coordinator unreachable — direct terminals may remain available; fleet controls unavailable"
            }
            Self::AuthRevoked => "This browser's access was revoked — sign in again",
        }
    }

    /// Whether a Reconnect button would do anything. A revoked credential is
    /// not a stale socket; offering the button would be offering a no-op.
    pub const fn offers_reconnect(self) -> bool {
        matches!(
            self,
            Self::CoordUnreachable | Self::CoordUnreachableDirectLive
        )
    }

    /// The status the banner's dot and rule carry: a warning while a direct
    /// terminal survives the outage, an error otherwise.
    const fn status(self) -> &'static str {
        match self {
            Self::CoordUnreachableDirectLive => "warn",
            _ => "error",
        }
    }

    /// The colour role of the banner's bottom rule, paired with [`Self::status`].
    const fn rule_color(self) -> &'static str {
        match self {
            Self::CoordUnreachableDirectLive => "var(--status-warn)",
            _ => "var(--md-sys-color-error)",
        }
    }
}

/// One reading of the browser and the store, everything the verdict needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerInputs {
    /// The coordinator rejected this browser's credential.
    pub auth_revoked: bool,
    /// `navigator.onLine`.
    pub browser_online: bool,
    /// `SyncState::idle_ms` on the core's clock: `None` with no open socket.
    pub link_idle_ms: Option<u64>,
    /// A direct terminal still holds a current liveness proof.
    pub direct_live: bool,
}

/// The banner's verdict for one reading, or `None` when the client is healthy.
///
/// Stateless on purpose: a verdict computed only from the current reading
/// cannot outlive the outage that produced it. Any timestamp the banner kept
/// itself would go stale across an evaluation gap — a hidden tab, a suspended
/// laptop — and hold the banner up over a live link.
pub fn banner_verdict(inputs: BannerInputs) -> Option<BannerReason> {
    if inputs.auth_revoked {
        return Some(BannerReason::AuthRevoked);
    }
    if !inputs.browser_online {
        return Some(BannerReason::Offline);
    }
    if sync_link_answering(inputs.link_idle_ms) {
        return None;
    }
    Some(if inputs.direct_live {
        BannerReason::CoordUnreachableDirectLive
    } else {
        BannerReason::CoordUnreachable
    })
}

/// The banner, or nothing when the client is healthy.
#[component]
pub fn ConnectionBanner() -> Element {
    let pump = use_store();
    let mut reason = use_signal(|| None::<BannerReason>);

    // A hidden tab is not evaluated: nobody can see the banner, and its last
    // verdict is replaced on the first tick after it is shown again.
    let watched = pump.clone();
    use_future(move || {
        let pump = watched.clone();
        async move {
            loop {
                if page_visible() {
                    let verdict = read_verdict(&pump);
                    if *reason.peek() != verdict {
                        reason.set(verdict);
                    }
                }
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
                style: format!("display: flex; align-items: center; justify-content: center; gap: var(--md-space-3); padding: var(--md-space-2) var(--md-space-4); border-bottom: var(--workbench-border-width) solid {}; color: var(--md-sys-color-on-surface); font: var(--md-body-s-weight) var(--md-body-s-size)/var(--md-body-s-line) var(--md-font);", verdict.rule_color()),
                StatusDot { status: verdict.status().to_owned() }
                span { "{verdict.message()}" }
                if verdict.offers_reconnect() {
                    Button {
                        variant: ButtonVariant::Secondary,
                        size: ButtonSize::Sm,
                        "data-testid": "connection-banner-reconnect",
                        onclick: move |_| {
                            pump.dispatch(ClientEvent::SyncTransportControl(TransportControl::Reconnect));
                            reason.set(None);
                        },
                        "Reconnect"
                    }
                }
            }
        }
    }
}

/// Read the browser and the store once, on the core's clock: the link's last
/// frame is stamped on that timeline, so any other clock makes `idle_ms` noise.
fn read_verdict(pump: &Pump) -> Option<BannerReason> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    banner_verdict(BannerInputs {
        auth_revoked: store.sync.auth_revoked,
        browser_online: crate::platform::network::browser_online(),
        link_idle_ms: store.sync.idle_ms(core.clock().now_ms()),
        direct_live: has_liveness_qualified_direct_terminal(store),
    })
}
