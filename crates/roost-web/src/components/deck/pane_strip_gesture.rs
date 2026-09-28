//! A pane strip's tab drag, as a gesture: arm past the drag threshold, walk
//! the dragged tab across its neighbours, then on release either tile it onto
//! another pane, spring it into its new slot and commit the order, or drop
//! it back. Called by `pane_strip` on a tab's pointer-down. Ports the pointer
//! handlers of `apps/web/src/components/deck/PaneStrip.tsx`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use super::deck_dom::{self, Listeners};
use super::pane_strip_drag::TabDrag;
use crate::motion::drag_threshold::drag_armed;

/// Where a strip reports a drag.
#[derive(Clone)]
pub struct StripDragOutlets {
    /// The strip's current tab order.
    pub tab_ids: Vec<String>,
    /// Commit a new order.
    pub on_reorder: EventHandler<Vec<String>>,
    /// The pointer moved during a drag, in client px.
    pub on_drag_move: Option<EventHandler<(f64, f64)>>,
    /// Released: `true` when the deck tiled the tab onto a pane.
    pub on_tile_drop: Option<Callback<(String, f64, f64), bool>>,
    /// The drag is over.
    pub on_drag_end: Option<EventHandler<()>>,
}

/// The strip's live drag and the resources it holds.
#[derive(Clone, Copy)]
pub struct StripGesture {
    /// The drag, while one runs.
    pub drag: Signal<Option<TabDrag>>,
    /// The rail the tabs are measured in.
    pub rail: Signal<Option<Rc<MountedData>>>,
    listeners: CopyValue<Rc<RefCell<Option<Listeners>>>>,
    #[cfg(target_arch = "wasm32")]
    settle: CopyValue<Rc<RefCell<Option<crate::motion::spring::SpringAnimation>>>>,
}

impl std::fmt::Debug for StripDragOutlets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("StripDragOutlets").field("tab_ids", &self.tab_ids).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for StripGesture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("StripGesture").field("drag", &*self.drag.peek()).finish_non_exhaustive()
    }
}

impl StripGesture {
    /// The gesture state for one strip.
    pub fn use_strip_gesture() -> Self {
        let listeners: CopyValue<Rc<RefCell<Option<Listeners>>>> = use_hook(|| CopyValue::new(Rc::default()));
        #[cfg(target_arch = "wasm32")]
        let settle: CopyValue<Rc<RefCell<Option<crate::motion::spring::SpringAnimation>>>> =
            use_hook(|| CopyValue::new(Rc::default()));
        use_drop(move || {
            listeners.read().borrow_mut().take();
        });
        Self {
            drag: use_signal(|| None),
            rail: use_signal(|| None),
            listeners,
            #[cfg(target_arch = "wasm32")]
            settle,
        }
    }

    /// Stop a spring still settling a previous release.
    pub fn cancel_settle(&self) {
        #[cfg(target_arch = "wasm32")]
        self.settle.read().borrow_mut().take();
    }

    /// A primary-button press on tab `id` at `index`, at client (`x`, `y`).
    pub fn press(&self, id: String, index: usize, start: (f64, f64), outlets: StripDragOutlets) {
        self.cancel_settle();
        let gesture = *self;
        let mut drag = self.drag;
        let rail = self.rail;
        let outlets = Rc::new(outlets);
        let on_move = {
            let outlets = Rc::clone(&outlets);
            let id = id.clone();
            move |x: f64, y: f64| {
                let current = drag.peek().clone();
                let next = match current {
                    Some(active) => Some(active.moved(x - start.0)),
                    None if drag_armed(start.0, start.1, x, y) => {
                        let rects = rail.peek().as_deref().map(deck_dom::tab_rects).unwrap_or_default();
                        TabDrag::arm(id.clone(), index, x - start.0, rects)
                    }
                    None => return,
                };
                drag.set(next);
                if let Some(report) = outlets.on_drag_move {
                    report.call((x, y));
                }
            }
        };
        let listeners = Rc::clone(&*self.listeners.read());
        let detach = {
            let listeners = Rc::clone(&listeners);
            move || {
                if let Some(active) = listeners.borrow().as_ref() {
                    active.remove();
                }
            }
        };
        let on_up = {
            let detach = detach.clone();
            let outlets = Rc::clone(&outlets);
            move |x: f64, y: f64| {
                detach();
                gesture.release(&outlets, x, y);
            }
        };
        let on_cancel = move || {
            detach();
            drag.set(None);
            if let Some(end) = outlets.on_drag_end {
                end.call(());
            }
        };
        *listeners.borrow_mut() = Listeners::window_pointer_drag(on_move, on_up, on_cancel);
    }

    fn release(&self, outlets: &StripDragOutlets, x: f64, y: f64) {
        let mut drag = self.drag;
        let Some(current) = drag.peek().clone() else { return };
        let tiled = outlets
            .on_tile_drop
            .is_some_and(|tile| tile.call((current.id.clone(), x, y)));
        if let Some(end) = outlets.on_drag_end {
            end.call(());
        }
        if tiled {
            deck_dom::swallow_next_click();
            drag.set(None);
            return;
        }
        if current.to_idx == current.from_idx {
            drag.set(None);
            return;
        }
        deck_dom::swallow_next_click();
        let ordered = current.reordered(&outlets.tab_ids);
        let on_reorder = outlets.on_reorder;
        tracing::debug!(target: "deck", from = current.from_idx, to = current.to_idx, "tab reordered");
        if deck_dom::reduced_motion() {
            on_reorder.call(ordered);
            drag.set(None);
            return;
        }
        drag.set(Some(TabDrag { released: true, ..current.clone() }));
        self.spring_to_rest(current, move || {
            on_reorder.call(ordered);
            drag.set(None);
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn spring_to_rest(&self, current: TabDrag, commit: impl FnOnce() + 'static) {
        use crate::motion::spring::{SPRING_SNAP, SpringState, animate_spring};

        let mut drag = self.drag;
        let rest = current.resting_dx();
        let animation = animate_spring(
            SpringState { position: current.dx, velocity: 0.0 },
            rest,
            SPRING_SNAP,
            move |position| {
                let next = drag.peek().clone().map(|active| TabDrag { dx: position, released: true, ..active });
                drag.set(next);
            },
            commit,
        );
        *self.settle.read().borrow_mut() = Some(animation);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spring_to_rest(&self, _current: TabDrag, commit: impl FnOnce() + 'static) {
        commit();
    }
}
