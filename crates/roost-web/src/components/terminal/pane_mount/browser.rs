//! The pane's browser plumbing: its one timer (armed at the earliest deadline
//! any of its state machines holds), its one animation frame, the display's
//! scroll/pointer listeners, the ResizeObserver, the page-lifecycle and font
//! listeners, and the pager's history reads. Every callback holds a `Weak`,
//! so a pane that dropped is a no-op, never a trap. Ports the listener halves
//! of `apps/web/src/components/terminal/cell-terminal-lifecycle.ts`,
//! `cell-terminal-renderer.ts` and `cell-terminal-document-lifecycle.ts`.

use std::rc::{Rc, Weak};

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, EventTarget, ResizeObserver};

use super::actions::{self, PaneAction, ViewportCtx, state_flags, with_state};
use super::{PaneShared, paint};
use crate::components::terminal::document_lifecycle::{
    DOCUMENT_LIFECYCLE_EVENTS, LifecycleAction, LifecycleTarget, classify, lifecycle_action,
};

type Listener = Closure<dyn FnMut(Event)>;

/// The pane's live browser handles.
#[derive(Default)]
pub(in crate::components::terminal) struct PaneBrowser {
    listeners: Vec<(EventTarget, &'static str, bool, Listener)>,
    timer: Option<(i32, u64)>,
    timer_callback: Option<Closure<dyn FnMut()>>,
    frame: Option<i32>,
    frame_callback: Option<Closure<dyn FnMut(f64)>>,
    resize_observer: Option<(ResizeObserver, Closure<dyn FnMut()>)>,
}

impl PaneBrowser {
    /// Hold a listener until `detach` removes it.
    pub(super) fn keep_listener(
        &mut self,
        target: EventTarget,
        kind: &'static str,
        capture: bool,
        listener: Listener,
    ) {
        self.listeners.push((target, kind, capture, listener));
    }
}

/// Milliseconds since the Unix epoch.
pub(super) fn now_ms() -> u64 {
    js_sys::Date::now().max(0.0) as u64
}

/// The document is visible.
pub(super) fn page_visible() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| document.visibility_state() == web_sys::VisibilityState::Visible)
}

/// The deck measures zero: every pane leaves layout for one tick.
pub(super) fn deck_box_collapsed() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector("[data-testid=\"terminal-deck\"]").ok()?)
        .is_some_and(|deck| deck.client_width() == 0 || deck.client_height() == 0)
}

fn listen(
    shared: &Rc<PaneShared>,
    target: &EventTarget,
    kind: &'static str,
    capture: bool,
    handler: fn(&PaneShared, &Event),
) {
    let weak: Weak<PaneShared> = Rc::downgrade(shared);
    let listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            handler(&shared, &event);
        }
    });
    let _ = target.add_event_listener_with_callback_and_bool(
        kind,
        listener.as_ref().unchecked_ref(),
        capture,
    );
    shared
        .browser
        .borrow_mut()
        .listeners
        .push((target.clone(), kind, capture, listener));
}

/// Attach every pane listener, the timer and frame callbacks, and the observer.
pub(super) fn attach(shared: &Rc<PaneShared>) {
    let weak = Rc::downgrade(shared);
    let timer = Closure::<dyn FnMut()>::new(move || {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            shared.browser.borrow_mut().timer = None;
            on_timer(&shared);
        }
    });
    let weak = Rc::downgrade(shared);
    let frame = Closure::<dyn FnMut(f64)>::new(move |_timestamp: f64| {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            shared.browser.borrow_mut().frame = None;
            paint::on_animation_frame(&shared);
        }
    });
    {
        let mut browser = shared.browser.borrow_mut();
        browser.timer_callback = Some(timer);
        browser.frame_callback = Some(frame);
    }
    let display: &EventTarget = shared.display.as_ref();
    listen(shared, display, "scroll", false, |shared, _| super::scroll::on_scroll(shared));
    listen(shared, display, "pointerdown", true, |shared, _| {
        shared.state.borrow_mut().pointer_gestures += 1;
    });
    if let Some(window) = web_sys::window() {
        let target: &EventTarget = window.as_ref();
        listen(shared, target, "pointerup", true, |shared, _| super::scroll::on_pointer_settled(shared));
        listen(shared, target, "pointercancel", true, |shared, _| super::scroll::on_pointer_settled(shared));
        listen(shared, target, "resize", false, |shared, _| schedule_viewport(shared));
        if let Some(document) = window.document() {
            for (kind, owner) in DOCUMENT_LIFECYCLE_EVENTS {
                let target: EventTarget = match owner {
                    LifecycleTarget::Document => document.clone().into(),
                    LifecycleTarget::Window => window.clone().into(),
                };
                listen(shared, &target, kind, false, on_lifecycle);
            }
            if let Ok(fonts) = js_sys::Reflect::get(document.as_ref(), &"fonts".into())
                && let Ok(fonts) = fonts.dyn_into::<EventTarget>()
            {
                listen(shared, &fonts, "loadingdone", false, |shared, _| on_fonts_settled(shared));
                listen(shared, &fonts, "loadingerror", false, |shared, _| on_fonts_settled(shared));
            }
        }
    }
    let weak = Rc::downgrade(shared);
    let resized = Closure::<dyn FnMut()>::new(move || {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            super::scroll::on_box_resize(&shared);
            schedule_viewport(&shared);
        }
    });
    if let Ok(observer) = ResizeObserver::new(resized.as_ref().unchecked_ref()) {
        observer.observe(shared.display.as_ref());
        shared.browser.borrow_mut().resize_observer = Some((observer, resized));
    }
}

/// Remove every listener and cancel the timer and frame.
pub(super) fn detach(shared: &PaneShared) {
    let mut browser = shared.browser.borrow_mut();
    for (target, kind, capture, listener) in browser.listeners.drain(..) {
        let _ = target.remove_event_listener_with_callback_and_bool(
            kind,
            listener.as_ref().unchecked_ref(),
            capture,
        );
    }
    if let Some((observer, _)) = browser.resize_observer.take() {
        observer.disconnect();
    }
    if let Some(window) = web_sys::window() {
        if let Some((handle, _)) = browser.timer.take() {
            window.clear_timeout_with_handle(handle);
        }
        if let Some(handle) = browser.frame.take() {
            let _ = window.cancel_animation_frame(handle);
        }
    }
}

/// Ask for the next animation frame, once.
pub(super) fn request_frame(shared: &PaneShared) {
    let mut browser = shared.browser.borrow_mut();
    if browser.frame.is_some() || shared.disposed.get() {
        return;
    }
    let (Some(window), Some(callback)) = (web_sys::window(), browser.frame_callback.as_ref()) else {
        return;
    };
    browser.frame = window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .ok();
}

/// Re-arm the one timer at the earliest deadline, and the frame when the
/// viewport retry waits on one.
pub(super) fn rearm(shared: &PaneShared) {
    if shared.disposed.get() {
        return;
    }
    let (due, wants_frame) = {
        let state = shared.state.borrow();
        let due = [
            state.viewport.next_deadline_ms(),
            state.presentation.next_deadline_ms(),
            state.dom_repair.next_deadline_ms(),
            state.offline.next_deadline_ms(),
            state.follow_settle_due_ms,
            state.cursor_poll.due_at_ms(),
        ]
        .into_iter()
        .flatten()
        .min();
        (due, state.viewport.wants_animation_frame())
    };
    if wants_frame {
        request_frame(shared);
    }
    let mut browser = shared.browser.borrow_mut();
    if browser.timer.map(|(_, at)| at) == due {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Some((handle, _)) = browser.timer.take() {
        window.clear_timeout_with_handle(handle);
    }
    let (Some(due), Some(callback)) = (due, browser.timer_callback.as_ref()) else {
        return;
    };
    let delay = i32::try_from(due.saturating_sub(now_ms())).unwrap_or(i32::MAX);
    if let Ok(handle) = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), delay)
    {
        browser.timer = Some((handle, due));
    }
}

fn on_timer(shared: &PaneShared) {
    let now = now_ms();
    with_state(shared, |state, actions| {
        let mut host = ViewportCtx::new(shared, state_flags(state), actions);
        state.viewport.on_deadline(now, &mut host);
    });
    paint::on_deadlines(shared, now);
    super::cursor_report::on_deadline(shared, now);
}

/// Publish once the resize burst settles, while the view is active.
pub(super) fn schedule_viewport(shared: &PaneShared) {
    let now = now_ms();
    with_state(shared, |state, _| {
        if state.flags.view_active() && state.page_visible {
            state.viewport.schedule(now);
        }
    });
}

fn on_fonts_settled(shared: &PaneShared) {
    shared.cell.set(None);
    shared.renderer.borrow_mut().invalidate_row_height();
    actions::publish_viewport_now(shared);
}

fn on_lifecycle(shared: &PaneShared, event: &Event) {
    let visible = page_visible();
    let Some(edge) = classify(&event.type_(), visible) else {
        return;
    };
    let now = now_ms();
    let view_active = {
        let mut state = shared.state.borrow_mut();
        state.page_visible = visible;
        let view_active = state.flags.view_active();
        state.backfill.set_active(visible && view_active);
        view_active
    };
    let action = lifecycle_action(edge, visible, view_active);
    tracing::debug!(target: "terminal", session_id = %shared.session_id, ?edge, ?action,
        "terminal document lifecycle");
    with_state(shared, |state, actions| {
        let mut host = ViewportCtx::new(shared, state_flags(state), actions);
        match action {
            LifecycleAction::Park => state.viewport.park(&mut host),
            LifecycleAction::Withdraw => state.viewport.withdraw(now, false, &mut host),
            LifecycleAction::Republish => {
                state.viewport.publish_now(now, &mut host);
                actions.push(PaneAction::RepublishView);
            }
            LifecycleAction::Ignore => {}
        }
    });
    paint::refresh_presentation(shared);
    paint::request_owed_paint(shared);
    super::interactions::sync_foreground(shared);
}

pub(super) use super::backfill_io::perform_backfill;
