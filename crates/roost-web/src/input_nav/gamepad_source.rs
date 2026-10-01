//! Gamepad API adapter: polls the first standard-mapping pad once per frame,
//! turns held buttons and axes into [`PadAction`]s (`pad_mapper`), publishes
//! the live held state the controller map highlights, and hands each poll's
//! intents to the App's callback. The polling loop exists ONLY while a
//! standard pad is connected and controller mode is not off, so a desktop
//! without one pays nothing — that decision is [`PadPollGate`], native-tested.
//! Called by the App (`install_gamepad_source`). Depends on `pad_mapper`,
//! `modality`, `modality_dom`. Ported from `apps/web/src/browser/gamepadSource.ts`.

use crate::input_nav::modality::ModeChoice;
use crate::input_nav::pad_mapper::PadSnapshot;

/// What the poll loop does after a refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTransition {
    /// Request the first frame.
    Start,
    /// Cancel the pending frame, forget holds, clear the held state.
    Stop,
    /// Leave the loop as it is.
    Keep,
}

/// One refresh's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadRefresh {
    /// Whether the pad count crossed zero in either direction (worth a log line).
    pub connection_changed: bool,
    /// What the loop does now.
    pub transition: PollTransition,
}

/// Whether the poll loop should be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadPollGate {
    pad_count: usize,
    polling: bool,
}

impl PadPollGate {
    /// No pads, not polling.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the loop is running.
    pub fn is_polling(&self) -> bool {
        self.polling
    }

    /// Re-decide after a connection event or a controller-choice change. The
    /// loop runs while at least one standard pad is connected and the choice is
    /// not `Off`; `Auto` polls so that the first real press can latch the mode.
    pub fn refresh(&mut self, standard_pads: usize, choice: ModeChoice) -> PadRefresh {
        let connection_changed = (standard_pads == 0) != (self.pad_count == 0);
        self.pad_count = standard_pads;
        let should_poll = standard_pads > 0 && choice != ModeChoice::Off;
        let transition = match (should_poll, self.polling) {
            (true, false) => PollTransition::Start,
            (false, true) => PollTransition::Stop,
            _ => PollTransition::Keep,
        };
        self.polling = should_poll;
        PadRefresh {
            connection_changed,
            transition,
        }
    }
}

/// One `navigator.getGamepads()` entry as plain data, plus the rule for
/// whether the poll may read it.
///
/// The entry is read through [`js_sys::Reflect`] rather than cast to
/// `web_sys::Gamepad`: that cast brand-checks against the browser's
/// `Gamepad` constructor, so any object merely SHAPED like a pad — a stand-in
/// under test, a polyfill, a wrapper from another realm — is dropped with no
/// error anywhere and the controller silently does nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct PadReading {
    connected: bool,
    standard: bool,
    snapshot: PadSnapshot,
}

impl PadReading {
    /// Read one entry's fields. `mapping` is the raw `GamepadMappingType`
    /// string; anything but `standard` leaves the pad unpolled.
    pub fn from_parts(connected: bool, mapping: &str, buttons: Vec<bool>, axes: Vec<f64>) -> Self {
        Self {
            connected,
            standard: mapping == STANDARD_MAPPING,
            snapshot: PadSnapshot { buttons, axes },
        }
    }

    /// Whether the poll loop may read this pad.
    pub fn is_pollable(&self) -> bool {
        self.connected && self.standard
    }

    /// The state one poll turns into intents.
    pub fn snapshot(&self) -> &PadSnapshot {
        &self.snapshot
    }
}

/// The only `GamepadMappingType` whose buttons this port is bound for.
pub const STANDARD_MAPPING: &str = "standard";

#[cfg(target_arch = "wasm32")]
pub use browser::{GamepadSourceGuard, install_gamepad_source};

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};

    use dioxus::prelude::{ReadableExt as _, Signal, WritableExt as _};
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;

    use super::{PadPollGate, PadReading, PollTransition};
    use crate::input_nav::dom_read;
    use crate::input_nav::modality::NavModality;
    use crate::input_nav::modality_dom::note_pad_activity;
    use crate::input_nav::pad_bindings::PadAction;
    use crate::input_nav::pad_mapper::{HeldPublisher, PadHeld, PadHoldState};

    const CONNECTION_EVENTS: [&str; 2] = ["gamepadconnected", "gamepaddisconnected"];

    #[derive(Default)]
    struct LoopState {
        gate: PadPollGate,
        holds: PadHoldState,
        publisher: HeldPublisher,
        frame: Option<i32>,
    }

    /// Where one poll's actions go; the shell installs the navigator's handler.
    type ActionSink = RefCell<Box<dyn FnMut(&[PadAction])>>;

    struct Source {
        modality: Signal<NavModality>,
        held: Signal<PadHeld>,
        on_actions: ActionSink,
        state: RefCell<LoopState>,
        poll: RefCell<Option<Closure<dyn FnMut()>>>,
        connection: RefCell<Option<Closure<dyn FnMut()>>>,
    }

    /// Stops the loop and removes the connection listeners when dropped.
    pub struct GamepadSourceGuard {
        source: Rc<Source>,
    }

    impl std::fmt::Debug for GamepadSourceGuard {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("GamepadSourceGuard")
                .field("polling", &self.source.state.borrow().gate.is_polling())
                .finish()
        }
    }

    impl GamepadSourceGuard {
        /// Re-decide whether to poll. The App calls this whenever the pad
        /// choice changes, so flipping the Settings picker starts or stops the
        /// loop at once.
        pub fn refresh_pads(&self) {
            refresh_pads(&self.source);
        }
    }

    impl Drop for GamepadSourceGuard {
        fn drop(&mut self) {
            if let Some(window) = web_sys::window() {
                if let Some(frame) = self.source.state.borrow_mut().frame.take() {
                    let _ = window.cancel_animation_frame(frame);
                }
                if let Some(listener) = self.source.connection.borrow().as_ref() {
                    for event in CONNECTION_EVENTS {
                        let _ = window.remove_event_listener_with_callback(
                            event,
                            listener.as_ref().unchecked_ref(),
                        );
                    }
                }
            }
            let mut held = self.source.held;
            if let Ok(mut value) = held.try_write() {
                *value = PadHeld::default();
            }
            tracing::info!(target: "input_nav", "gamepad source uninstalled");
        }
    }

    /// Poll standard-mapping pads for the lifetime of the guard, handing each
    /// poll's intents to `on_actions` and publishing held state into `held`.
    pub fn install_gamepad_source(
        modality: Signal<NavModality>,
        held: Signal<PadHeld>,
        on_actions: impl FnMut(&[PadAction]) + 'static,
    ) -> GamepadSourceGuard {
        let source = Rc::new(Source {
            modality,
            held,
            on_actions: RefCell::new(Box::new(on_actions)),
            state: RefCell::new(LoopState::default()),
            poll: RefCell::new(None),
            connection: RefCell::new(None),
        });
        *source.poll.borrow_mut() = Some(weak_callback(&source, poll_once));
        let connection = weak_callback(&source, refresh_pads);
        if let Some(window) = web_sys::window() {
            for event in CONNECTION_EVENTS {
                let _ = window
                    .add_event_listener_with_callback(event, connection.as_ref().unchecked_ref());
            }
        }
        *source.connection.borrow_mut() = Some(connection);
        // The install-time run matters: after a reload the pad is already
        // connected and `gamepadconnected` may never fire again.
        refresh_pads(&source);
        tracing::info!(target: "input_nav", "gamepad source installed");
        GamepadSourceGuard { source }
    }

    fn weak_callback(source: &Rc<Source>, run: fn(&Rc<Source>)) -> Closure<dyn FnMut()> {
        let weak: Weak<Source> = Rc::downgrade(source);
        Closure::new(move || {
            if let Some(source) = weak.upgrade() {
                run(&source);
            }
        })
    }

    fn refresh_pads(source: &Rc<Source>) {
        let pads = standard_pads().len();
        let choice = source.modality.peek().pad_choice();
        let refresh = source.state.borrow_mut().gate.refresh(pads, choice);
        if refresh.connection_changed {
            tracing::info!(target: "input_nav", pads, mapping = "standard", "pad.connected");
        }
        match refresh.transition {
            PollTransition::Start => request_frame(source),
            PollTransition::Stop => {
                let cleared = {
                    let mut state = source.state.borrow_mut();
                    if let (Some(frame), Some(window)) = (state.frame.take(), web_sys::window()) {
                        let _ = window.cancel_animation_frame(frame);
                    }
                    state.holds.clear();
                    state.publisher.publish(None)
                };
                publish_held(source, cleared);
                tracing::info!(target: "input_nav", pads, choice = choice.as_str(), "pad poll stopped");
            }
            PollTransition::Keep => {}
        }
    }

    fn request_frame(source: &Rc<Source>) {
        let frame = match (web_sys::window(), source.poll.borrow().as_ref()) {
            (Some(window), Some(poll)) => window
                .request_animation_frame(poll.as_ref().unchecked_ref())
                .ok(),
            _ => None,
        };
        source.state.borrow_mut().frame = frame;
    }

    fn poll_once(source: &Rc<Source>) {
        request_frame(source);
        let readings = standard_pads();
        let snapshot = readings.first().map(PadReading::snapshot);
        let (published, actions) = {
            let mut state = source.state.borrow_mut();
            let published = state.publisher.publish(snapshot);
            let actions = snapshot
                .map(|snapshot| state.holds.actions_to_fire(snapshot, dom_read::now_ms()))
                .unwrap_or_default();
            (published, actions)
        };
        publish_held(source, published);
        if actions.is_empty() {
            return;
        }
        // Latch `Auto` BEFORE dispatching: the first press must both flip the
        // modality and act, so nothing is swallowed to "warm up" the mode.
        note_pad_activity(source.modality);
        (source.on_actions.borrow_mut())(&actions);
    }

    fn publish_held(source: &Source, published: Option<PadHeld>) {
        if let Some(value) = published {
            let mut held = source.held;
            held.set(value);
        }
    }

    fn standard_pads() -> Vec<PadReading> {
        // No Gamepad API at all (older Safari) is a refused call, not a panic.
        let Some(Ok(pads)) = web_sys::window().map(|window| window.navigator().get_gamepads())
        else {
            return Vec::new();
        };
        let entries = pads.length() as usize;
        let pollable: Vec<PadReading> = pads
            .iter()
            .map(|pad| reading_of(&pad))
            .filter(PadReading::is_pollable)
            .collect();
        if pollable.is_empty() {
            tracing::debug!(target: "input_nav", entries, "no connected standard pad among the reported entries");
        }
        pollable
    }

    fn reading_of(pad: &JsValue) -> PadReading {
        let buttons = array_property(pad, "buttons");
        let axes = array_property(pad, "axes");
        PadReading::from_parts(
            bool_property(pad, "connected").unwrap_or(false),
            string_property(pad, "mapping")
                .as_deref()
                .unwrap_or_default(),
            buttons
                .iter()
                .map(|button| bool_property(&button, "pressed").unwrap_or(false))
                .collect(),
            axes.iter()
                .map(|axis| axis.as_f64().unwrap_or(0.0))
                .collect(),
        )
    }

    /// `Reflect::get` walks the prototype chain, so a real `Gamepad`'s
    /// prototype getters read exactly as a stand-in's own properties do.
    fn property(value: &JsValue, key: &str) -> Option<JsValue> {
        js_sys::Reflect::get(value, &JsValue::from_str(key)).ok()
    }

    fn bool_property(value: &JsValue, key: &str) -> Option<bool> {
        property(value, key)?.as_bool()
    }

    fn string_property(value: &JsValue, key: &str) -> Option<String> {
        property(value, key)?.as_string()
    }

    fn array_property(value: &JsValue, key: &str) -> js_sys::Array {
        match property(value, key) {
            Some(array) if array.is_array() => js_sys::Array::from(&array),
            _ => js_sys::Array::new(),
        }
    }
}
