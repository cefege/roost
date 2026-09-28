//! The browser half of one pointer resize: capture, window listeners and the
//! coalesced animation frame around a `PointerResizeSession`. Ports
//! `beginPointerResizeDrag` from `apps/web/src/lib/resizeDrag.ts`; called by
//! `SidebarResizer` and the deck's pane dividers.
//!
//! Order on finish is v2's: frame cancelled, listeners removed, capture
//! released, commit, release callback, THEN the owner token — so suppression
//! holds through the committed layout.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, PointerEvent, Window};

use super::resize_drag::{PointerResizeSession, ResizeDrag, ResizeDragToken};

type Listener = Closure<dyn FnMut(web_sys::Event)>;

/// What a pointer resize reports to its owner.
pub struct PointerResizeCallbacks<T> {
    /// The geometry a move event means.
    pub geometry_for: Box<dyn Fn(&PointerEvent) -> T>,
    /// Apply geometry live, once per frame.
    pub on_move: Box<dyn FnMut(T)>,
    /// Apply the final geometry.
    pub on_commit: Box<dyn FnMut(T)>,
    /// The drag is over, committed or not.
    pub on_release: Box<dyn FnMut()>,
}

struct Running<T> {
    session: PointerResizeSession<T>,
    token: Option<ResizeDragToken>,
    frame_id: Option<i32>,
    callbacks: PointerResizeCallbacks<T>,
    listeners: Vec<(bool, &'static str, Listener)>,
    frame: Option<Closure<dyn FnMut(f64)>>,
}

/// A live pointer resize. `dispose` aborts it without committing.
#[derive(Clone)]
pub struct PointerResizeHandle {
    abort: Rc<dyn Fn()>,
}

impl PointerResizeHandle {
    /// Abort the drag; the latest sample is discarded. Idempotent.
    pub fn dispose(&self) {
        (self.abort)();
    }
}

/// Begin a pointer resize on `target` for `pointer_id`.
pub fn begin_pointer_resize_drag<T: Copy + 'static>(
    drag: ResizeDrag,
    target: Element,
    pointer_id: i32,
    initial: T,
    callbacks: PointerResizeCallbacks<T>,
) -> Option<PointerResizeHandle> {
    let window = web_sys::window()?;
    let running = Rc::new(RefCell::new(Running {
        session: PointerResizeSession::new(pointer_id, initial),
        token: Some(drag.begin()),
        frame_id: None,
        callbacks,
        listeners: Vec::new(),
        frame: None,
    }));
    let finish: Rc<dyn Fn(bool)> = {
        let running = Rc::clone(&running);
        let window = window.clone();
        let target = target.clone();
        Rc::new(move |commit| finish_drag(&running, &window, &target, drag, commit))
    };
    let frame = {
        let running = Rc::clone(&running);
        Closure::<dyn FnMut(f64)>::new(move |_| {
            let mut state = running.borrow_mut();
            state.frame_id = None;
            if let Some(geometry) = state.session.frame() {
                (state.callbacks.on_move)(geometry);
            }
        })
    };
    let on_move = {
        let running = Rc::clone(&running);
        let window = window.clone();
        Listener::new(move |event: web_sys::Event| {
            let Some(event) = event.dyn_ref::<PointerEvent>() else {
                return;
            };
            let mut state = running.borrow_mut();
            let geometry = (state.callbacks.geometry_for)(event);
            if state.session.sample(event.pointer_id(), geometry)
                && let Some(frame) = state.frame.as_ref()
            {
                state.frame_id = window.request_animation_frame(frame.as_ref().unchecked_ref()).ok();
            }
        })
    };
    let pointer_end = |finish: Rc<dyn Fn(bool)>, running: Rc<RefCell<Running<T>>>| {
        Listener::new(move |event: web_sys::Event| {
            let ends = event
                .dyn_ref::<PointerEvent>()
                .is_some_and(|event| running.borrow().session.ends_on(event.pointer_id()));
            if ends {
                finish(true);
            }
        })
    };
    let blur = {
        let finish = Rc::clone(&finish);
        Listener::new(move |_| finish(true))
    };
    let window_target: &web_sys::EventTarget = window.as_ref();
    let listeners: Vec<(bool, &'static str, Listener)> = vec![
        (true, "pointermove", on_move),
        (true, "pointerup", pointer_end(Rc::clone(&finish), Rc::clone(&running))),
        (true, "pointercancel", pointer_end(Rc::clone(&finish), Rc::clone(&running))),
        (true, "blur", blur),
        (false, "lostpointercapture", pointer_end(Rc::clone(&finish), Rc::clone(&running))),
    ];
    for (on_window, name, listener) in &listeners {
        let host: &web_sys::EventTarget = if *on_window { window_target } else { target.as_ref() };
        let _ = host.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
    }
    {
        let mut state = running.borrow_mut();
        state.listeners = listeners;
        state.frame = Some(frame);
    }
    // Synthetic pointers can carry an id the browser does not know.
    let _ = target.set_pointer_capture(pointer_id);
    tracing::debug!(target: "motion", pointer_id, "pointer resize began");
    Some(PointerResizeHandle {
        abort: Rc::new(move || finish(false)),
    })
}

fn finish_drag<T: Copy + 'static>(
    running: &Rc<RefCell<Running<T>>>,
    window: &Window,
    target: &Element,
    drag: ResizeDrag,
    commit: bool,
) {
    let Some(settlement) = running.borrow_mut().session.finish(commit) else {
        return;
    };
    let (listeners, frame_id) = {
        let mut state = running.borrow_mut();
        (std::mem::take(&mut state.listeners), state.frame_id.take())
    };
    if let Some(frame_id) = frame_id.filter(|_| settlement.cancel_frame) {
        let _ = window.cancel_animation_frame(frame_id);
    }
    let window_target: &web_sys::EventTarget = window.as_ref();
    for (on_window, name, listener) in &listeners {
        let host: &web_sys::EventTarget = if *on_window { window_target } else { target.as_ref() };
        let _ = host.remove_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
    }
    let pointer_id = running.borrow().session.pointer_id();
    if target.has_pointer_capture(pointer_id) {
        let _ = target.release_pointer_capture(pointer_id);
    }
    let token = {
        let mut state = running.borrow_mut();
        if let Some(geometry) = settlement.commit {
            (state.callbacks.on_commit)(geometry);
        }
        (state.callbacks.on_release)();
        state.frame = None;
        state.token.take()
    };
    if let Some(token) = token {
        drag.release(token);
    }
    // The listeners are dropped only now, after this call no longer runs
    // inside any of them.
    drop(listeners);
    tracing::debug!(target: "motion", committed = settlement.commit.is_some(), "pointer resize finished");
}
