//! The requester's state and the hook that drives it: one ceremony for the
//! pairing surface, whether that surface is the unauthorized gate or `/pair`.
//!
//! Ported from `apps/web/src/components/pairing/PairingRequesterProvider.tsx`
//! and the controller it creates. v2 put the controller in a context ABOVE the
//! access gate so a `checking → unauthorized → authorized` transition never
//! disposed it; this reads the same tab-scoped record on every mount instead,
//! which is the record v2's controller also reloads from after a document load.
//! The behaviour a reader sees is unchanged — a ceremony in flight survives
//! both a reload and a gate switch — without a provider the app root would own.

mod driver;

use std::cell::Cell;

use dioxus::prelude::*;
use roost_client_core::client::auth::PairPollStatus;

use crate::pump::{Pump, use_pump};

pub use driver::{CreateOutcome, POLL_INTERVAL_MS, RequesterRig};

/// Everything the request card draws, in one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequesterState {
    /// The ceremony's server stage, or `Idle` before a request exists.
    pub status: PairPollStatus,
    /// A local refusal to show in place of the stage — a code that did not
    /// match, a confirmation whose answer was lost.
    pub failure: Option<String>,
    /// The refusal the page reports separately, because it retired the whole
    /// ceremony rather than one step of it.
    pub request_failure: Option<String>,
    /// Whether the ceremony ended in a failure retrying cannot fix.
    ///
    /// Its own flag rather than a status: no poll will ever report it, and
    /// folding it into `PairPollStatus` would put a client-only state into a
    /// type whose whole job is naming a server stage.
    pub failed: bool,
    /// Whether a call is in flight.
    pub busy: bool,
    /// What the reader has typed into the verification field.
    pub verification_code: String,
}

impl RequesterState {
    /// No request has been asked for.
    pub fn idle() -> Self {
        Self {
            status: PairPollStatus::Idle,
            failure: None,
            request_failure: None,
            failed: false,
            busy: false,
            verification_code: String::new(),
        }
    }

    /// A request was asked for a moment ago and has not been answered.
    pub fn asking() -> Self {
        Self {
            status: PairPollStatus::Pending,
            busy: true,
            ..Self::idle()
        }
    }
}

/// The one requester ceremony a pairing surface owns.
#[derive(Clone)]
pub struct PairingRequester {
    state: Signal<RequesterState>,
    rig: RequesterRig,
    confirm_now: EventHandler<()>,
}

impl std::fmt::Debug for PairingRequester {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingRequester")
            .field("state", &self.state.peek())
            .finish_non_exhaustive()
    }
}

/// Two requesters are the same ceremony when they own the same state signal.
///
/// `#[component]` derives props equality, and a card that took a requester by
/// value would otherwise re-render every descendant on every parent render.
impl PartialEq for PairingRequester {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

/// The requester ceremony for the surface that is rendering now.
pub fn use_pairing_requester() -> PairingRequester {
    let pump: Pump = use_pump();
    let state = use_signal(RequesterState::idle);
    // Generation one is the mount, which restores but never mints; every later
    // generation is one reader asking for a request.
    let generation = use_signal(|| 1_u64);
    let rig = use_hook(|| RequesterRig::new(pump, state, generation));
    let confirm_now = use_callback({
        let rig = rig.clone();
        move |()| {
            let rig = rig.clone();
            spawn(async move { rig.confirm().await });
        }
    });
    start_ceremony_loop(rig.clone(), generation);
    PairingRequester {
        state,
        rig,
        confirm_now,
    }
}

/// Run the ceremony loop, and re-run it when a newer request supersedes it.
///
/// `use_future` spawns ONCE and never re-reads its closure, so the reactive half
/// is this effect: a generation change cancels the loop that owned the previous
/// request and starts the one that owns this. The generation it last started is
/// remembered so the mount run is not immediately restarted on top of itself.
fn start_ceremony_loop(rig: RequesterRig, generation: Signal<u64>) {
    let mut ceremony = use_future({
        let rig = rig.clone();
        move || {
            let mine = *generation.read();
            let rig = rig.clone();
            async move { drive(rig, mine).await }
        }
    });
    let started = use_hook(|| Cell::new(*generation.peek()));
    use_effect(move || {
        let mine = *generation.read();
        if started.get() == mine {
            return;
        }
        started.set(mine);
        ceremony.restart();
    });
}

impl PairingRequester {
    /// The card's state, subscribed for this render.
    pub fn state(&self) -> Signal<RequesterState> {
        self.state
    }

    /// Ask for a request, superseding whatever was on screen.
    pub fn start(&self) {
        self.rig.start();
    }

    /// Abandon the ceremony without asking for another.
    pub fn clear(&self) {
        self.rig.clear();
    }

    /// Record what the reader typed, and drop the refusal the last attempt
    /// earned — that sentence was about the previous code, not this one.
    pub fn update_verification_code(&self, value: String) {
        let mut next = self.rig.state();
        next.verification_code = value;
        next.failure = None;
        let mut state = self.state;
        state.set(next);
    }

    /// Send the typed code.
    pub fn confirm(&self) {
        self.confirm_now.call(());
    }
}

/// Create the request, then poll it until the ceremony ends.
///
/// The loop IS the future, so unmounting the surface drops it: a page that
/// navigated away mid-ceremony leaves no timer behind, which is the guarantee
/// v2 gets from `onCleanup` cancelling its scheduled action.
async fn drive(rig: RequesterRig, generation: u64) {
    if rig.is_stale(generation) {
        return;
    }
    match rig.begin(generation).await {
        CreateOutcome::Fatal(message) => {
            tracing::error!(target: "auth", %message, "pairing request could not be created");
            rig.fail(message);
        }
        CreateOutcome::Acknowledged => {}
        CreateOutcome::Transient | CreateOutcome::Idle => return,
    }
    loop {
        crate::components::terminal::dom::sleep_ms(POLL_INTERVAL_MS).await;
        if rig.is_stale(generation) || rig.is_finished() {
            return;
        }
        // A confirmation in flight is not a reason to stop. The marker is held
        // across the poll that recovers a lost confirmation as well, because
        // that poll is the one question the ceremony still has to ask — and the
        // next tick after it has to find out what it committed.
        if rig.is_confirming() {
            continue;
        }
        rig.poll_once().await;
    }
}
