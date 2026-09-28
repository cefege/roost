//! The compact deck bar swipe: the commit-to-switch decision, the momentum
//! settle timing, the end-of-list affordances, and the arm/track/end state
//! machine the touch adapter drives. Read by `terminal_deck_swipe` (wasm) and
//! `deck_swipe_style`; the card-dismiss pair is read by the terminal card grid.
//! Pure; ports `apps/web/src/lib/deckSwipe.ts` and the transitions of
//! `apps/web/src/components/deck/terminal-deck-swipe.ts`.

/// A deliberate drag commits at 40% of the width.
pub const SWITCH_DIST_FRAC: f64 = 0.4;
/// A directional flick commits at this speed, px/ms.
pub const SWITCH_FLING_VEL: f64 = 0.6;
/// A flick must still have travelled 12% of the width.
pub const SWITCH_FLING_MIN_FRAC: f64 = 0.12;
/// Settle speed: ms per full screen width of travel.
pub const ANIMATION_SPEED_SCREEN_MS: f64 = 500.0;
/// A card swipe dismisses at this travel, px.
pub const CARD_DISMISS_PX: f64 = 144.0;
/// A card flick dismisses at this speed, px/ms.
pub const CARD_FLING_VEL: f64 = 0.5;
/// A card flick must still have travelled this far, px.
pub const CARD_FLING_MIN_PX: f64 = 24.0;
/// The new-terminal container-transform reveal, ms.
pub const NEW_BLOOM_MS: u64 = 300;

/// Which way a swipe walks the tab list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDirection {
    /// Finger moving left: the next tab.
    Next,
    /// Finger moving right: the previous tab.
    Previous,
}

impl SwipeDirection {
    /// `1` for next, `-1` for previous.
    pub fn sign(self) -> f64 {
        match self {
            Self::Next => 1.0,
            Self::Previous => -1.0,
        }
    }

    /// The direction a finger travelling `delta_x` arms.
    pub fn of_travel(delta_x: f64) -> Self {
        if delta_x < 0.0 { Self::Next } else { Self::Previous }
    }
}

/// What a swipe does when released past the commit line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeMode {
    /// A real neighbour: slide to it.
    Slide,
    /// Past the last tab: open a new terminal.
    NewTerminal,
    /// Before the first tab: open the workspace drawer.
    Workspace,
}

/// Whether the finger is down or the slot is settling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipePhase {
    /// Finger-follow.
    Track,
    /// Animating to its resting place.
    Settle,
}

/// Where a settling slot lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleTarget {
    /// The switch happens.
    Commit,
    /// The slot springs back.
    Cancel,
}

/// One live swipe.
#[derive(Debug, Clone, PartialEq)]
pub struct Swipe {
    /// Tracking or settling.
    pub phase: SwipePhase,
    /// The route's session when the swipe armed.
    pub current_id: String,
    /// The adjacent tab, or `None` at an end.
    pub neighbor_id: Option<String>,
    /// The armed direction.
    pub dir: SwipeDirection,
    /// Live finger travel, px, clamped to the width.
    pub offset: f64,
    /// Slide, or an end affordance.
    pub mode: SwipeMode,
    /// Set only once released.
    pub settle_target: Option<SettleTarget>,
    /// The settle's duration, ms; `None` while tracking.
    pub settle_ms: Option<u64>,
}

/// What the deck does once a settle finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwipeCompletion {
    /// Select this tab.
    SelectNeighbor(String),
    /// Open a new terminal in the focused pane.
    NewTerminal,
    /// Nothing; the slot sprang back.
    Cancelled,
}

/// A release: the settling swipe, what follows it, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct SwipeRelease {
    /// The settling state to paint.
    pub settling: Swipe,
    /// What runs when it lands.
    pub completion: SwipeCompletion,
    /// How long to wait, ms, slack included.
    pub delay_ms: u64,
}

/// Extra wait past a settle so its transition has painted its last frame.
pub const SETTLE_SLACK_MS: u64 = 20;

/// Commit only when released IN the armed direction AND either a real distance
/// drag or a directional flick past a small travel floor; that floor and the
/// direction check keep a weak or backward flick from switching.
pub fn should_commit_switch(dx: f64, velocity: f64, dir: SwipeDirection, width: f64) -> bool {
    if SwipeDirection::of_travel(dx) != dir {
        return false;
    }
    let travel = dx.abs();
    let distance_ok = travel >= width * SWITCH_DIST_FRAC;
    let fling_ok =
        -dir.sign() * velocity >= SWITCH_FLING_VEL && travel >= width * SWITCH_FLING_MIN_FRAC;
    distance_ok || fling_ok
}

/// A card dismisses on a full-travel drag or a flick the same way as the drag.
pub fn should_dismiss_card(dx: f64, vx: f64) -> bool {
    let travel = dx.abs();
    if travel >= CARD_DISMISS_PX {
        return true;
    }
    vx.abs() >= CARD_FLING_VEL && travel >= CARD_FLING_MIN_PX && vx.signum() == dx.signum()
}

/// A dragged card's opacity: 1 at rest, 0.2 at the dismiss threshold.
pub fn card_swipe_alpha(dx: f64) -> f64 {
    (1.0 - (0.8 * dx.abs()) / CARD_DISMISS_PX).max(0.2)
}

/// Constant-speed settle: `remaining` px always takes 500ms per width.
pub fn settle_duration_ms(remaining: f64, width: f64) -> u64 {
    if width <= 0.0 {
        return 0;
    }
    (ANIMATION_SPEED_SCREEN_MS * remaining.abs() / width).round() as u64
}

/// What a swipe becomes: a slide with a neighbour, else the end affordance.
pub fn end_mode(dir: SwipeDirection, has_neighbor: bool) -> SwipeMode {
    match (has_neighbor, dir) {
        (true, _) => SwipeMode::Slide,
        (false, SwipeDirection::Next) => SwipeMode::NewTerminal,
        (false, SwipeDirection::Previous) => SwipeMode::Workspace,
    }
}

/// The new-terminal pull's progress: 0 at rest, 1 at the commit distance.
pub fn new_fab_progress(offset: f64, width: f64) -> f64 {
    if width <= 0.0 {
        return 0.0;
    }
    (offset.abs() / (width * SWITCH_DIST_FRAC)).min(1.0)
}

/// Arm a swipe over `tabs` (the flat phone order) from the route's session.
/// `None` when the route's session is not a tab or a settle is running.
pub fn arm_swipe(
    delta_x: f64,
    tabs: &[String],
    active_session_id: Option<&str>,
    current: Option<&Swipe>,
) -> Option<Swipe> {
    if current.is_some_and(|swipe| swipe.phase == SwipePhase::Settle) {
        return None;
    }
    let active = active_session_id?;
    let index = tabs.iter().position(|tab| tab == active)?;
    let dir = SwipeDirection::of_travel(delta_x);
    let neighbor_id = match dir {
        SwipeDirection::Next => tabs.get(index + 1).cloned(),
        SwipeDirection::Previous => index.checked_sub(1).and_then(|prev| tabs.get(prev).cloned()),
    };
    Some(Swipe {
        phase: SwipePhase::Track,
        current_id: active.to_owned(),
        mode: end_mode(dir, neighbor_id.is_some()),
        neighbor_id,
        dir,
        offset: delta_x,
        settle_target: None,
        settle_ms: None,
    })
}

/// Follow the finger, clamped to the deck width.
pub fn track_swipe(swipe: &Swipe, delta_x: f64, width: f64) -> Swipe {
    let mut next = swipe.clone();
    if swipe.phase == SwipePhase::Track {
        next.offset = delta_x.clamp(-width.max(0.0), width.max(0.0));
    }
    next
}

/// Release a tracking swipe: settle to the neighbour, bloom the new terminal,
/// or spring back.
pub fn release_swipe(swipe: &Swipe, delta_x: f64, velocity: f64, width: f64) -> Option<SwipeRelease> {
    if swipe.phase != SwipePhase::Track {
        return None;
    }
    let travelled = swipe.offset.abs();
    let mut settling = swipe.clone();
    settling.phase = SwipePhase::Settle;
    if should_commit_switch(delta_x, velocity, swipe.dir, width) {
        settling.settle_target = Some(SettleTarget::Commit);
        if swipe.mode == SwipeMode::NewTerminal {
            settling.settle_ms = Some(NEW_BLOOM_MS);
            return Some(SwipeRelease {
                settling,
                completion: SwipeCompletion::NewTerminal,
                delay_ms: NEW_BLOOM_MS + SETTLE_SLACK_MS,
            });
        }
        let settle_ms = settle_duration_ms(width - travelled, width);
        settling.offset = -swipe.dir.sign() * width;
        settling.settle_ms = Some(settle_ms);
        let completion = swipe
            .neighbor_id
            .clone()
            .map_or(SwipeCompletion::Cancelled, SwipeCompletion::SelectNeighbor);
        return Some(SwipeRelease { settling, completion, delay_ms: settle_ms + SETTLE_SLACK_MS });
    }
    let settle_ms = settle_duration_ms(travelled, width);
    settling.settle_target = Some(SettleTarget::Cancel);
    settling.offset = 0.0;
    settling.settle_ms = Some(settle_ms);
    Some(SwipeRelease {
        settling,
        completion: SwipeCompletion::Cancelled,
        delay_ms: settle_ms + SETTLE_SLACK_MS,
    })
}

/// The neighbour whose own bar rides in beside the current one: a real
/// slide on a compact host only.
pub fn bar_neighbor_id(swipe: Option<&Swipe>, compact: bool) -> Option<&str> {
    let swipe = swipe?;
    if !compact || swipe.mode != SwipeMode::Slide {
        return None;
    }
    swipe.neighbor_id.as_deref()
}
