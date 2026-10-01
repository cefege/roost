//! The desktop sidebar boundary: a focusable `separator` that resizes the
//! sidebar by pointer drag (captured, frame-coalesced), by arrow/Home/End, and
//! resets on a double press. Ports `apps/web/src/components/layout/SidebarResizer.tsx`;
//! mounted by `SidebarRegion` while the sidebar is expanded. Widths are clamped
//! by the store (`store::ui::clamp_sidebar_width`) through `ShellIntent::SetSidebarWidth`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::ui::{SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN};

use crate::pump::use_store;

/// One keyboard step, in pixels.
pub const RESIZER_KEY_STEP_PX: u32 = 10;

/// The width a key asks for from `current`, or `None` when the key is not the
/// resizer's (the event is then left alone).
pub fn keyboard_width(key: &str, current: u32) -> Option<u32> {
    match key {
        "ArrowLeft" => Some(current.saturating_sub(RESIZER_KEY_STEP_PX)),
        "ArrowRight" => Some(current.saturating_add(RESIZER_KEY_STEP_PX)),
        "Home" => Some(SIDEBAR_WIDTH_MIN),
        "End" => Some(SIDEBAR_WIDTH_MAX),
        _ => None,
    }
}

/// The width a drag that started at `start_width` with the pointer at
/// `start_x` asks for with the pointer now at `x` (negative clamps to zero; the
/// store clamps to its range).
pub fn drag_width(start_width: u32, start_x: f64, x: f64) -> u32 {
    let width = f64::from(start_width) + (x - start_x);
    if width <= 0.0 {
        0
    } else {
        width.round() as u32
    }
}

/// How long two presses may be apart and still be one double press, ms.
pub const DOUBLE_PRESS_MS: f64 = 500.0;

/// How far apart two presses may be and still be one double press, px.
pub const DOUBLE_PRESS_SLOP_PX: f64 = 4.0;

/// Where and when one press landed, on the event's own clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressPoint {
    /// Client x.
    pub x: f64,
    /// Client y.
    pub y: f64,
    /// Milliseconds since the page's time origin.
    pub time_ms: f64,
}

/// What one primary press asks the store for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressOutcome {
    /// Begin a drag, seeded with the width the store reads at the press.
    BeginDrag { start_width: u32 },
    /// The second press of a double press: put the width back.
    Reset { px: u32 },
}

/// The resizer's press memory.
///
/// The reset is read off the press rather than a `dblclick` event because Dioxus
/// matches a handler by the browser's event name against the attribute with its
/// `on` stripped: `ondoubleclick` strips to `doubleclick` while the browser
/// sends `dblclick`, so that handler never hears the event it names.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PressTracker {
    /// The last primary press, spent once a second one pairs with it.
    last: Option<PressPoint>,
    /// A gesture that began at the last press may still write.
    live: bool,
}

impl PressTracker {
    /// What the press at `point` asks for, with the store reading
    /// `current_width` right now.
    pub fn press(&mut self, point: PressPoint, current_width: u32) -> PressOutcome {
        if self.pairs_with(point) {
            // The pairing is spent, and the gesture the first press began stops
            // admitting writes BEFORE the reset lands: a commit already in
            // flight must not put the width back where the drag left it.
            self.last = None;
            self.live = false;
            return PressOutcome::Reset {
                px: SIDEBAR_WIDTH_DEFAULT,
            };
        }
        self.last = Some(point);
        self.live = true;
        PressOutcome::BeginDrag {
            start_width: current_width,
        }
    }

    /// Whether a gesture may still write the width.
    pub fn is_live(&self) -> bool {
        self.live
    }

    fn pairs_with(&self, point: PressPoint) -> bool {
        let Some(last) = self.last else {
            return false;
        };
        let elapsed = point.time_ms - last.time_ms;
        (0.0..=DOUBLE_PRESS_MS).contains(&elapsed)
            && (point.x - last.x).abs() <= DOUBLE_PRESS_SLOP_PX
            && (point.y - last.y).abs() <= DOUBLE_PRESS_SLOP_PX
    }
}

/// The resizer.
#[component]
pub fn SidebarResizer() -> Element {
    let pump = use_store();
    let width = pump.core().borrow().store().ui.sidebar_width;
    #[cfg(target_arch = "wasm32")]
    let presses = use_signal(PressTracker::default);
    #[cfg(target_arch = "wasm32")]
    let drag = crate::motion::resize_drag::use_resize_drag();
    #[cfg(target_arch = "wasm32")]
    let active = use_signal(|| None::<crate::motion::resize_pointer::PointerResizeHandle>);
    #[cfg(target_arch = "wasm32")]
    use_drop(move || {
        if let Some(handle) = active.peek().as_ref() {
            handle.dispose();
        }
    });
    let key_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    let press_pump = pump.clone();
    rsx! {
        div {
            class: "workbench-sidebar-resizer",
            "data-testid": "sidebar-resizer",
            role: "separator",
            "aria-label": "Resize sidebar",
            "aria-orientation": "vertical",
            "aria-valuemin": "{SIDEBAR_WIDTH_MIN}",
            "aria-valuemax": "{SIDEBAR_WIDTH_MAX}",
            "aria-valuenow": "{width}",
            tabindex: "0",
            onpointerdown: move |event: PointerEvent| {
                #[cfg(target_arch = "wasm32")]
                press::on_pointer_down(&event, press_pump.clone(), drag, presses, active);
                #[cfg(not(target_arch = "wasm32"))]
                let _ = event;
            },
            onkeydown: move |event: KeyboardEvent| {
                if let Some(px) = keyboard_width(&event.key().to_string(), width) {
                    event.prevent_default();
                    key_pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px }));
                }
            },
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod press {
    use dioxus::prelude::*;
    use dioxus::web::WebEventExt as _;
    use roost_client_core::ClientEvent;
    use roost_client_core::store::shell_intent::ShellIntent;
    use wasm_bindgen::JsCast as _;

    use super::{PressOutcome, PressPoint, PressTracker, drag_width};
    use crate::motion::resize_drag::ResizeDrag;
    use crate::motion::resize_pointer::{
        PointerResizeCallbacks, PointerResizeHandle, begin_pointer_resize_drag,
    };
    use crate::pump::Pump;

    fn body_style(cursor: &str, user_select: &str) {
        if let Some(body) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body())
        {
            let style = body.style();
            let _ = style.set_property("cursor", cursor);
            let _ = style.set_property("user-select", user_select);
        }
    }

    /// Decide what one primary press on the resizer does, then do it.
    pub(super) fn on_pointer_down(
        event: &PointerEvent,
        pump: Pump,
        drag: ResizeDrag,
        mut presses: Signal<PressTracker>,
        mut active: Signal<Option<PointerResizeHandle>>,
    ) {
        let Some(native) = event.try_as_web_event() else {
            return;
        };
        if native.button() != 0 {
            return;
        }
        // Read here, not from the value the last render captured: a width that
        // landed in the store since then is the width this gesture belongs to,
        // and seeding from the older one drags the sidebar back to it.
        let current = pump.core().borrow().store().ui.sidebar_width;
        let outcome = presses.write().press(
            PressPoint {
                x: f64::from(native.client_x()),
                y: f64::from(native.client_y()),
                time_ms: native.time_stamp(),
            },
            current,
        );
        match outcome {
            PressOutcome::Reset { px } => {
                retire(active);
                pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px }));
            }
            PressOutcome::BeginDrag { start_width } => {
                let gesture = begin(event, start_width, drag, pump, presses, active);
                if let Some(handle) = gesture {
                    active.set(Some(handle));
                }
            }
        }
    }

    /// Stop a gesture still in flight, so it cannot write after the reset.
    fn retire(mut active: Signal<Option<PointerResizeHandle>>) {
        let handle = active.write().take();
        if let Some(handle) = handle {
            handle.dispose();
        }
    }

    fn begin(
        event: &PointerEvent,
        start_width: u32,
        drag: ResizeDrag,
        pump: Pump,
        presses: Signal<PressTracker>,
        mut active: Signal<Option<PointerResizeHandle>>,
    ) -> Option<PointerResizeHandle> {
        if active.peek().is_some() {
            return None;
        }
        let native: web_sys::PointerEvent = event.try_as_web_event()?;
        let target = native
            .current_target()?
            .dyn_into::<web_sys::Element>()
            .ok()?;
        let start_x = f64::from(native.client_x());
        body_style("col-resize", "none");
        let set_width = move |pump: Pump| {
            move |px: u32| {
                if presses.read().is_live() {
                    pump.dispatch(ClientEvent::Shell(ShellIntent::SetSidebarWidth { px }));
                }
            }
        };
        begin_pointer_resize_drag(
            drag,
            target,
            native.pointer_id(),
            start_width,
            PointerResizeCallbacks {
                geometry_for: Box::new(move |moved| {
                    drag_width(start_width, start_x, f64::from(moved.client_x()))
                }),
                on_move: Box::new(set_width(pump.clone())),
                on_commit: Box::new(set_width(pump)),
                on_release: Box::new(move || {
                    active.set(None);
                    body_style("", "");
                }),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: PressPoint = PressPoint {
        x: 348.0,
        y: 12.0,
        time_ms: 4_000.0,
    };

    fn press_after(dx: f64, dy: f64, ms: f64) -> PressPoint {
        PressPoint {
            x: FIRST.x + dx,
            y: FIRST.y + dy,
            time_ms: FIRST.time_ms + ms,
        }
    }

    #[test]
    fn the_second_press_resets_to_the_default_and_retires_the_drag() {
        let mut tracker = PressTracker::default();
        assert_eq!(
            tracker.press(FIRST, 300),
            PressOutcome::BeginDrag { start_width: 300 }
        );
        assert!(tracker.is_live(), "the first press begins a drag");
        assert_eq!(
            tracker.press(press_after(1.0, 0.0, 120.0), 310),
            PressOutcome::Reset {
                px: SIDEBAR_WIDTH_DEFAULT,
            }
        );
        assert!(
            !tracker.is_live(),
            "a commit in flight at the reset must not put the width back"
        );
    }

    #[test]
    fn a_reset_spends_the_pairing_so_a_third_press_begins_a_drag() {
        let mut tracker = PressTracker::default();
        let _ = tracker.press(FIRST, 300);
        let _ = tracker.press(press_after(0.0, 0.0, 100.0), 300);
        assert_eq!(
            tracker.press(press_after(0.0, 0.0, 200.0), SIDEBAR_WIDTH_DEFAULT),
            PressOutcome::BeginDrag {
                start_width: SIDEBAR_WIDTH_DEFAULT,
            }
        );
    }

    #[test]
    fn each_drag_starts_from_the_width_the_store_reads_now() {
        let mut tracker = PressTracker::default();
        assert_eq!(
            tracker.press(FIRST, 300),
            PressOutcome::BeginDrag { start_width: 300 }
        );
        // Long after the first press, so the pair is spent: the next drag is
        // seeded from the width the store holds, not from the 300 this
        // element last rendered.
        assert_eq!(
            tracker.press(press_after(0.0, 0.0, 9_000.0), 410),
            PressOutcome::BeginDrag { start_width: 410 }
        );
    }

    #[test]
    fn presses_apart_in_time_or_space_are_two_drags() {
        let mut late = PressTracker::default();
        let _ = late.press(FIRST, 300);
        assert_eq!(
            late.press(press_after(0.0, 0.0, DOUBLE_PRESS_MS + 1.0), 300),
            PressOutcome::BeginDrag { start_width: 300 }
        );
        let mut wide = PressTracker::default();
        let _ = wide.press(FIRST, 300);
        assert_eq!(
            wide.press(press_after(DOUBLE_PRESS_SLOP_PX + 1.0, 0.0, 100.0), 300),
            PressOutcome::BeginDrag { start_width: 300 }
        );
    }
}
