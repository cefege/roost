//! The compact deck's swipe binding: one touch listener on the deck for its
//! life, reading the newest render, that arms, tracks, settles and springs
//! back the tab swipe (`deck_swipe`), hands a backward pull at the first tab
//! to the workspace drawer, runs the landing operation once the settle has
//! painted, and drops the swipe once the route has moved past it. Called by
//! `terminal_deck`; touches are read by `deck_swipe_touch`. Ports the state
//! half of `apps/web/src/components/deck/terminal-deck-swipe.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use super::deck_dom::{self, Listeners, Timeout};
use super::deck_swipe::{
    Swipe, SwipeCompletion, SwipeMode, SwipePhase, SwipeRelease, arm_swipe, cancel_swipe,
    new_fab_progress, release_swipe, route_outlives_swipe, should_commit_switch, track_swipe,
};
use super::deck_swipe_touch::{SwipeTouchTracker, TouchStep};
use super::terminal_deck_operations::DeckOperations;
use crate::motion::edge_swipe_drawer::open_offset_px;

/// What a touch reads from the newest render.
#[derive(Debug, Clone)]
pub struct SwipeFrame {
    /// Whether the host paints one pane.
    pub compact: bool,
    /// The deck width, px.
    pub width: f64,
    /// The phone's flat tab order.
    pub mobile_tabs: Vec<String>,
    /// The route's session.
    pub active_session_id: Option<String>,
    /// This render's operations.
    pub operations: DeckOperations,
}

/// The timers one settle owns, held only so a newer settle's replacing them
/// cancels them: its completion once the transition has painted, and the drop
/// of a slide whose route never followed.
#[derive(Debug, Default)]
struct SettleTimers {
    _completion: Option<Timeout>,
    _landing_deadline: Option<Timeout>,
}

/// Bind the swipe to the deck element once it mounts, and drop a swipe the
/// route or the folder has moved out from under.
pub fn use_deck_swipe(
    swipe: Signal<Option<Swipe>>,
    deck_element: Signal<Option<Rc<MountedData>>>,
    frame: SwipeFrame,
    folder_key: Option<String>,
    followed_session_id: Option<String>,
) {
    let followed = followed_session_id;
    let latest: Rc<RefCell<Option<SwipeFrame>>> = use_hook(Rc::default);
    *latest.borrow_mut() = Some(frame);
    let listeners: Rc<RefCell<Option<Listeners>>> = use_hook(Rc::default);
    let timers: Rc<RefCell<SettleTimers>> = use_hook(Rc::default);
    use_effect(move || {
        let Some(deck) = deck_element.read().clone() else {
            return;
        };
        if listeners.borrow().is_some() {
            return;
        }
        let latest = Rc::clone(&latest);
        let timers = Rc::clone(&timers);
        let mut tracker = SwipeTouchTracker::default();
        let mut fab_armed = false;
        *listeners.borrow_mut() = Listeners::deck_touches(&deck, move |touch| {
            let Some(frame) = latest.borrow().clone() else {
                return false;
            };
            let step = tracker.step(touch, frame.compact);
            frame.apply(swipe, step, &mut fab_armed, &timers);
            step.consumes()
        });
    });
    use_effect(use_reactive((&followed,), move |(followed,)| {
        let Some(active) = followed else { return };
        let outlived = swipe
            .peek()
            .as_ref()
            .is_some_and(|current| route_outlives_swipe(current, &active));
        if outlived {
            tracing::debug!(target: "deck", followed = %active, "deck swipe dropped for the route");
            let mut clearing = swipe;
            clearing.set(None);
        }
    }));
    use_effect(use_reactive((&folder_key,), move |_| {
        if swipe.peek().is_some() {
            let mut clearing = swipe;
            clearing.set(None);
        }
    }));
}

impl SwipeFrame {
    fn apply(
        &self,
        swipe: Signal<Option<Swipe>>,
        step: TouchStep,
        fab_armed: &mut bool,
        timers: &Rc<RefCell<SettleTimers>>,
    ) {
        match step {
            TouchStep::Ignored => {}
            TouchStep::Armed { delta_x } => {
                *fab_armed = false;
                if !self.compact {
                    return;
                }
                let armed = arm_swipe(
                    delta_x,
                    &self.mobile_tabs,
                    self.active_session_id.as_deref(),
                    swipe.peek().as_ref(),
                );
                if let Some(armed) = armed {
                    tracing::debug!(target: "deck", mode = ?armed.mode, "deck swipe armed");
                    let mut arming = swipe;
                    arming.set(Some(armed));
                }
                self.track(swipe, delta_x, fab_armed);
            }
            TouchStep::Tracked { delta_x } => self.track(swipe, delta_x, fab_armed),
            TouchStep::Released { delta_x, velocity } => {
                self.release(swipe, delta_x, velocity, timers)
            }
            TouchStep::Cancelled => self.cancel(swipe, timers),
        }
    }

    fn track(&self, swipe: Signal<Option<Swipe>>, delta_x: f64, fab_armed: &mut bool) {
        let Some(current) = swipe.peek().clone() else {
            return;
        };
        let next = track_swipe(&current, delta_x, self.width);
        if next != current {
            let mut tracking = swipe;
            tracking.set(Some(next.clone()));
        }
        if next.mode == SwipeMode::Workspace {
            deck_dom::drawer_follow(open_offset_px(next.offset, deck_dom::viewport_width()));
        } else if next.mode == SwipeMode::NewTerminal
            && !*fab_armed
            && new_fab_progress(next.offset, self.width) >= 1.0
        {
            *fab_armed = true;
            deck_dom::vibrate(8);
        }
    }

    fn release(
        &self,
        swipe: Signal<Option<Swipe>>,
        delta_x: f64,
        velocity: f64,
        timers: &Rc<RefCell<SettleTimers>>,
    ) {
        let Some(current) = swipe.peek().clone() else {
            return;
        };
        if current.mode == SwipeMode::Workspace && current.phase == SwipePhase::Track {
            deck_dom::drawer_settle_open(should_commit_switch(
                delta_x,
                velocity,
                current.dir,
                deck_dom::viewport_width(),
            ));
            let mut clearing = swipe;
            clearing.set(None);
            return;
        }
        let Some(release) = release_swipe(&current, delta_x, velocity, self.width) else {
            return;
        };
        tracing::debug!(target: "deck", completion = ?release.completion, "deck swipe released");
        if release.completion == SwipeCompletion::NewTerminal {
            deck_dom::vibrate(12);
        }
        self.settle(swipe, release, timers);
    }

    fn cancel(&self, swipe: Signal<Option<Swipe>>, timers: &Rc<RefCell<SettleTimers>>) {
        let Some(current) = swipe.peek().clone() else {
            return;
        };
        if current.mode == SwipeMode::Workspace && current.phase == SwipePhase::Track {
            deck_dom::drawer_settle_open(false);
            let mut clearing = swipe;
            clearing.set(None);
            return;
        }
        let Some(release) = cancel_swipe(&current, self.width) else {
            return;
        };
        tracing::debug!(target: "deck", mode = ?current.mode, "deck swipe cancelled");
        self.settle(swipe, release, timers);
    }

    /// Paint the settle, run its completion once the transition has painted,
    /// and drop the swipe then, or, for a slide, once its route follows (the
    /// followed-session effect) or the landing grace runs out.
    fn settle(
        &self,
        swipe: Signal<Option<Swipe>>,
        release: SwipeRelease,
        timers: &Rc<RefCell<SettleTimers>>,
    ) {
        let drop_deadline_ms = release.drop_deadline_ms();
        let SwipeRelease {
            settling,
            completion,
            delay_ms,
        } = release;
        let mut painting = swipe;
        painting.set(Some(settling.clone()));
        let operations = self.operations.clone();
        let waits_for_route = completion.waits_for_route();
        let landed = settling.clone();
        let completion_timer = Timeout::after(timer_ms(delay_ms), move || {
            match completion {
                SwipeCompletion::SelectNeighbor(session_id) => operations.select(session_id),
                SwipeCompletion::NewTerminal => {
                    operations.new_tab(operations.focused_pane_id.clone().unwrap_or_default());
                }
                SwipeCompletion::Cancelled => {}
            }
            if !waits_for_route {
                drop_swipe_if_still(swipe, &landed);
            }
        });
        let landing_deadline = if waits_for_route {
            Timeout::after(timer_ms(drop_deadline_ms), move || {
                if drop_swipe_if_still(swipe, &settling) {
                    tracing::warn!(target: "deck", "deck swipe dropped; its route never followed");
                }
            })
        } else {
            None
        };
        *timers.borrow_mut() = SettleTimers {
            _completion: completion_timer,
            _landing_deadline: landing_deadline,
        };
    }
}

/// Drop the swipe if it is still the settle a timer was set for; a newer
/// swipe armed after the folder or the route cleared that one is left alone.
fn drop_swipe_if_still(swipe: Signal<Option<Swipe>>, settle: &Swipe) -> bool {
    if swipe.peek().as_ref() != Some(settle) {
        return false;
    }
    let mut clearing = swipe;
    clearing.set(None);
    true
}

fn timer_ms(delay_ms: u64) -> u32 {
    u32::try_from(delay_ms).unwrap_or(u32::MAX)
}
