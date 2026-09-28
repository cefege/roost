//! One pane's geometry claims: a resize burst publishes once after it settles,
//! an unmeasured box retries on the next frame then once after 100 ms, and a
//! zero-sized deck is absorbed by a 250 ms grace instead of withdrawing. Ports
//! `apps/web/tests/cellTerminalViewport.test.ts` and the timing cases of
//! `apps/web/tests/cellTerminalViewport.parkGrace.test.ts`.

use roost_web::components::terminal::viewport_publication::{ViewportHost, ViewportPublication};

struct Pane {
    width: u32,
    height: u32,
    active: bool,
    published: Vec<(u32, u32)>,
    withdrawals: u32,
}

impl ViewportHost for Pane {
    fn measure(&mut self) -> Option<(u32, u32)> {
        (self.width > 0 && self.height > 0).then_some((self.width / 10, self.height / 20))
    }
    fn should_publish_active(&self) -> bool {
        self.active
    }
    fn has_view(&self) -> bool {
        true
    }
    fn publish(&mut self, cols: u32, rows: u32) {
        self.published.push((cols, rows));
    }
    fn withdraw(&mut self) {
        self.withdrawals += 1;
    }
    fn release_paint_holds(&mut self) {}
}

struct Fixture {
    pane: Pane,
    viewport: ViewportPublication,
    now: u64,
}

impl Fixture {
    fn new() -> Self {
        Self {
            pane: Pane {
                width: 800,
                height: 400,
                active: true,
                published: Vec::new(),
                withdrawals: 0,
            },
            viewport: ViewportPublication::new(),
            now: 0,
        }
    }

    /// Advance the fake clock, firing every deadline in order as it passes.
    fn advance(&mut self, ms: u64) {
        let until = self.now + ms;
        while let Some(due) = self.viewport.next_deadline_ms().filter(|due| *due <= until) {
            self.now = due;
            self.viewport.on_deadline(due, &mut self.pane);
        }
        self.now = until;
    }

    fn flush_frame(&mut self) {
        self.viewport.on_animation_frame(self.now, &mut self.pane);
    }

    fn publish_now(&mut self) -> bool {
        self.viewport.publish_now(self.now, &mut self.pane)
    }
}

#[test]
fn a_resize_burst_publishes_the_settled_geometry_once_after_50_ms() {
    let mut fixture = Fixture::new();
    fixture.viewport.schedule(fixture.now);
    fixture.advance(25);
    (fixture.pane.width, fixture.pane.height) = (1_000, 500);
    fixture.viewport.schedule(fixture.now);
    fixture.advance(49);
    assert!(fixture.pane.published.is_empty());
    fixture.advance(1);
    assert_eq!(fixture.pane.published, [(100, 25)]);
    fixture.advance(500);
    assert_eq!(fixture.pane.published, [(100, 25)]);
}

#[test]
fn an_immediate_publication_cancels_a_pending_trailing_resize() {
    let mut fixture = Fixture::new();
    fixture.viewport.schedule(fixture.now);
    (fixture.pane.width, fixture.pane.height) = (900, 440);
    assert!(fixture.publish_now());
    assert_eq!(fixture.pane.published, [(90, 22)]);
    fixture.advance(100);
    assert_eq!(fixture.pane.published, [(90, 22)]);
}

#[test]
fn the_100_ms_retry_publishes_once_after_two_failures() {
    let mut fixture = Fixture::new();
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    assert!(fixture.viewport.wants_animation_frame());
    fixture.flush_frame();
    fixture.advance(99);
    assert!(fixture.pane.published.is_empty());
    fixture.pane.width = 800;
    fixture.advance(1);
    assert_eq!(fixture.pane.published, [(80, 20)]);
    fixture.advance(500);
    assert_eq!(fixture.pane.published, [(80, 20)]);
}

#[test]
fn park_and_withdraw_cancel_retries_before_a_later_activation() {
    let mut fixture = Fixture::new();
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.viewport.park(&mut fixture.pane);
    fixture.pane.width = 800;
    fixture.flush_frame();
    assert!(fixture.pane.published.is_empty());

    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.flush_frame();
    fixture.pane.active = false;
    fixture.viewport.publish_inactive(&mut fixture.pane);
    fixture.pane.width = 800;
    fixture.advance(100);
    assert!(fixture.pane.published.is_empty());

    fixture.pane.active = true;
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.pane.width = 800;
    fixture.flush_frame();
    assert_eq!(fixture.pane.published, [(80, 20)]);
}

#[test]
fn a_park_cancels_a_retry_already_escalated_to_its_timer() {
    let mut fixture = Fixture::new();
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.flush_frame();
    fixture.viewport.park(&mut fixture.pane);
    fixture.pane.width = 800;
    fixture.advance(100);
    assert!(fixture.pane.published.is_empty());
}

#[test]
fn a_positive_retry_coalesces_with_a_pending_trailing_resize() {
    let mut fixture = Fixture::new();
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.flush_frame();
    fixture.advance(50);
    fixture.viewport.schedule(fixture.now);
    fixture.pane.width = 800;
    fixture.advance(50);
    assert_eq!(fixture.pane.published, [(80, 20)]);
    fixture.advance(100);
    assert_eq!(fixture.pane.published, [(80, 20)]);
}

#[test]
fn a_new_retry_episode_starts_only_on_a_later_request() {
    let mut fixture = Fixture::new();
    fixture.pane.width = 0;
    assert!(!fixture.publish_now());
    fixture.flush_frame();
    fixture.advance(100);
    assert!(!fixture.viewport.wants_animation_frame());
    assert_eq!(fixture.viewport.next_deadline_ms(), None);

    assert!(!fixture.publish_now());
    assert!(fixture.viewport.wants_animation_frame());
    fixture.pane.width = 800;
    fixture.flush_frame();
    assert_eq!(fixture.pane.published, [(80, 20)]);
}

#[test]
fn a_pane_that_returns_inside_the_grace_is_never_withdrawn() {
    let mut fixture = Fixture::new();
    fixture.pane.active = false;
    fixture
        .viewport
        .withdraw(fixture.now, true, &mut fixture.pane);
    fixture.advance(120);
    assert_eq!(fixture.pane.withdrawals, 0);
    fixture.pane.active = true;
    assert!(fixture.publish_now());
    fixture.advance(1_000);
    assert_eq!(fixture.pane.withdrawals, 0);
    assert_eq!(fixture.pane.published.last(), Some(&(80, 20)));
}

#[test]
fn a_resize_inside_the_grace_cannot_park_the_graced_withdraw() {
    let mut fixture = Fixture::new();
    fixture.pane.active = false;
    fixture
        .viewport
        .withdraw(fixture.now, true, &mut fixture.pane);
    (fixture.pane.width, fixture.pane.height) = (0, 0);
    fixture.viewport.schedule(fixture.now);
    fixture.advance(60);
    assert_eq!(fixture.pane.withdrawals, 0);
}

#[test]
fn a_deck_that_stays_collapsed_withdraws_once_after_the_grace() {
    let mut fixture = Fixture::new();
    fixture.pane.active = false;
    fixture
        .viewport
        .withdraw(fixture.now, true, &mut fixture.pane);
    fixture.advance(249);
    assert_eq!(fixture.pane.withdrawals, 0);
    fixture.advance(1);
    assert_eq!(fixture.pane.withdrawals, 1);
    fixture.advance(5_000);
    assert_eq!(fixture.pane.withdrawals, 1);
}

#[test]
fn a_real_hide_withdraws_at_once_and_supersedes_an_armed_grace() {
    let mut fixture = Fixture::new();
    fixture.pane.active = false;
    fixture
        .viewport
        .withdraw(fixture.now, false, &mut fixture.pane);
    assert_eq!(fixture.pane.withdrawals, 1);

    let mut graced = Fixture::new();
    graced.pane.active = false;
    graced.viewport.withdraw(graced.now, true, &mut graced.pane);
    graced.viewport.park(&mut graced.pane);
    assert_eq!(graced.pane.withdrawals, 1);
    graced.advance(1_000);
    assert_eq!(graced.pane.withdrawals, 1);
}
