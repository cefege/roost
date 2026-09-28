//! A damped-spring solver for drag-follow and settle motion (tab reorder,
//! smooth scroll). Ports `apps/web/src/lib/spring.ts`; read by the deck's tab
//! strip. Physics runs in SECONDS: position in px, velocity in px/s.
//!
//! `spring_step` and `is_spring_at_rest` are pure; `animate_spring` is the one
//! frame driver, and it settles straight at the target under reduced motion.

/// A spring's constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringConfig {
    /// Pull toward the target.
    pub stiffness: f64,
    /// Resistance; `2 * sqrt(k * m)` is critical.
    pub damping: f64,
    /// Mass.
    pub mass: f64,
}

/// Where the spring is and how fast it moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringState {
    /// Pixels.
    pub position: f64,
    /// Pixels per second.
    pub velocity: f64,
}

/// Within this many pixels of the target counts as there.
pub const SPRING_REST_POSITION: f64 = 0.1;
/// Slower than this many pixels per second counts as stopped.
pub const SPRING_REST_VELOCITY: f64 = 1.0;

/// Crisp tab-reorder settle, slightly underdamped.
pub const SPRING_SNAP: SpringConfig = SpringConfig {
    stiffness: 700.0,
    damping: 45.0,
    mass: 1.0,
};

/// The damping that settles fastest without overshoot.
pub fn critical_damping(stiffness: f64, mass: f64) -> f64 {
    2.0 * (stiffness * mass).sqrt()
}

/// One semi-implicit Euler step toward `target`, `dt_ms` after the last one.
/// A zero or negative step changes nothing.
pub fn spring_step(
    state: SpringState,
    target: f64,
    config: SpringConfig,
    dt_ms: f64,
) -> SpringState {
    let dt = dt_ms.max(0.0) / 1000.0;
    if dt == 0.0 {
        return state;
    }
    let displacement = state.position - target;
    let accel = (-config.stiffness * displacement - config.damping * state.velocity) / config.mass;
    let velocity = state.velocity + accel * dt;
    SpringState {
        position: state.position + velocity * dt,
        velocity,
    }
}

/// Whether the spring is close enough and slow enough to stop.
pub fn is_spring_at_rest(state: SpringState, target: f64) -> bool {
    (state.position - target).abs() < SPRING_REST_POSITION
        && state.velocity.abs() < SPRING_REST_VELOCITY
}

/// The longest frame gap a step integrates, so a stalled tab does not launch
/// the spring past its target.
pub const MAX_FRAME_DT_MS: f64 = 64.0;

/// Drive a spring on animation frames until it rests; `on_frame` receives each
/// position and the exact target last. Dropping the returned handle cancels it.
#[cfg(target_arch = "wasm32")]
pub fn animate_spring(
    from: SpringState,
    target: f64,
    config: SpringConfig,
    on_frame: impl FnMut(f64) + 'static,
    on_done: impl FnOnce() + 'static,
) -> SpringAnimation {
    frames::animate(from, target, config, on_frame, on_done)
}

#[cfg(target_arch = "wasm32")]
pub use frames::SpringAnimation;

#[cfg(target_arch = "wasm32")]
mod frames {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    use super::{MAX_FRAME_DT_MS, SpringConfig, SpringState, is_spring_at_rest, spring_step};
    use crate::motion::view_transition::prefers_reduced_motion;

    type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

    /// A running spring. Dropping it stops the loop before the next frame.
    #[derive(Debug)]
    pub struct SpringAnimation {
        cancelled: Rc<Cell<bool>>,
    }

    impl Drop for SpringAnimation {
        fn drop(&mut self) {
            self.cancelled.set(true);
        }
    }

    pub(super) fn animate(
        from: SpringState,
        target: f64,
        config: SpringConfig,
        on_frame: impl FnMut(f64) + 'static,
        on_done: impl FnOnce() + 'static,
    ) -> SpringAnimation {
        let cancelled = Rc::new(Cell::new(false));
        let on_frame = Rc::new(RefCell::new(on_frame));
        let on_done = Rc::new(RefCell::new(Some(on_done)));
        let settle = {
            let on_frame = Rc::clone(&on_frame);
            let on_done = Rc::clone(&on_done);
            move || {
                (on_frame.borrow_mut())(target);
                if let Some(done) = on_done.borrow_mut().take() {
                    done();
                }
            }
        };
        let Some(window) = web_sys::window() else {
            return SpringAnimation { cancelled };
        };
        if prefers_reduced_motion() {
            // Deferred like the animated path so a cancel issued right after
            // this returns still wins.
            let stop = Rc::clone(&cancelled);
            let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
            let deferred = Closure::once(move |_: wasm_bindgen::JsValue| {
                if !stop.get() {
                    settle();
                }
            });
            let _ = promise.then(&deferred);
            deferred.forget();
            return SpringAnimation { cancelled };
        }
        let state = Cell::new(from);
        let last = Cell::new(None::<f64>);
        let callback: FrameCallback = Rc::new(RefCell::new(None));
        let stop = Rc::clone(&cancelled);
        let next = Rc::clone(&callback);
        let frame_window = window.clone();
        *callback.borrow_mut() = Some(Closure::new(move |now: f64| {
            if stop.get() {
                next.borrow_mut().take();
                return;
            }
            let dt_ms = last
                .get()
                .map_or(0.0, |previous| (now - previous).min(MAX_FRAME_DT_MS));
            last.set(Some(now));
            let stepped = spring_step(state.get(), target, config, dt_ms);
            state.set(stepped);
            if is_spring_at_rest(stepped, target) {
                next.borrow_mut().take();
                settle();
                return;
            }
            (on_frame.borrow_mut())(stepped.position);
            if let Some(frame) = next.borrow().as_ref() {
                let _ = frame_window.request_animation_frame(frame.as_ref().unchecked_ref());
            }
        }));
        if let Some(frame) = callback.borrow().as_ref() {
            let _ = window.request_animation_frame(frame.as_ref().unchecked_ref());
        }
        SpringAnimation { cancelled }
    }
}
