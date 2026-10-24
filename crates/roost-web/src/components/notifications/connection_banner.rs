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

use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::pump::{Pump, use_store};
use libero::{
    components::{Alert, Box, Button, Flex, Indicator, Text},
    sx::sx,
};

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

    const fn color(self) -> &'static str {
        match self {
            Self::CoordUnreachableDirectLive => "warning",
            _ => "error",
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
    let position_sx = use_hook(|| {
        sx().position("fixed")
            .with("left", "0")
            .with("right", "0")
            .with("top", "0")
            .z_index("50")
    });
    let banner_sx = use_hook(|| {
        sx().padding("xs")
            .border_radius("0")
            .with("border-bottom", "1px solid currentColor")
    });

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
        Box {
            "data-testid": "connection-banner",
            "data-banner-reason": verdict.as_str(),
            sx: position_sx,
            Alert { color: verdict.color(), radius: "0", sx: banner_sx,
                Flex { direction: "row", align: "center", justify: "center", gap: "sm",
                    Indicator { color: verdict.color(), aria_hidden: "true" }
                    Text { size: "xs", "{verdict.message()}" }
                    if verdict.offers_reconnect() {
                        Button { variant: "outlined", size: "sm",
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
