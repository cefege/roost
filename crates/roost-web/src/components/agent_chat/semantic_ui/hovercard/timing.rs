//! Per-view hover dwell and short-grace ownership.
//! Cards share warmth while another card is open and through its leave grace.
//! Dropping the membership releases the view without global tooltip state.

use std::cell::RefCell;
use std::rc::Rc;

pub(super) const CLOSE_DELAY_MS: u32 = 150;
const OPEN_DELAY_MS: u32 = 350;
const WARM_DELAY_MS: u32 = 200;

#[derive(Clone, Default)]
pub(crate) struct HoverCardScope(Rc<RefCell<WarmState>>);

#[derive(Default)]
struct WarmState {
    open: usize,
    warm_until_ms: Option<f64>,
}

pub(super) struct HoverMembership {
    scope: HoverCardScope,
    open: bool,
}

impl HoverCardScope {
    pub(super) fn membership(&self) -> HoverMembership {
        HoverMembership {
            scope: self.clone(),
            open: false,
        }
    }

    pub(super) fn delay_ms(&self, now_ms: f64) -> u32 {
        let state = self.0.borrow();
        if state.open > 0 || state.warm_until_ms.is_some_and(|until| now_ms < until) {
            WARM_DELAY_MS
        } else {
            OPEN_DELAY_MS
        }
    }
}

impl HoverMembership {
    pub(super) fn set_open(&mut self, open: bool, now_ms: f64) {
        if self.open == open {
            return;
        }
        self.open = open;
        let mut state = self.scope.0.borrow_mut();
        if open {
            state.open += 1;
        } else {
            state.open = state.open.saturating_sub(1);
            state.warm_until_ms = Some(now_ms + f64::from(CLOSE_DELAY_MS));
        }
    }
}

impl Drop for HoverMembership {
    fn drop(&mut self) {
        self.set_open(false, now_ms());
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |clock| clock.now())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn now_ms() -> f64 {
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_cards_share_only_their_views_open_and_leave_grace() {
        let scope = HoverCardScope::default();
        let other_view = HoverCardScope::default();
        let mut first = scope.membership();
        assert_eq!(scope.delay_ms(0.0), 350);
        first.set_open(true, 350.0);
        assert_eq!(scope.delay_ms(400.0), 200);
        assert_eq!(other_view.delay_ms(400.0), 350);
        first.set_open(false, 450.0);
        assert_eq!(scope.delay_ms(599.0), 200);
        assert_eq!(scope.delay_ms(600.0), 350);
    }

    #[test]
    fn repeated_render_does_not_leave_a_phantom_open_card() {
        let scope = HoverCardScope::default();
        let mut member = scope.membership();
        member.set_open(true, 1.0);
        member.set_open(true, 2.0);
        member.set_open(false, 3.0);
        member.set_open(false, 4.0);
        assert_eq!(scope.delay_ms(153.0), 350);
    }
}
