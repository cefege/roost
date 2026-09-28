//! The terminal deck's hook-held state: the measured deck box and desktop
//! strip height, the warm set, the observation it reports to the core, and the
//! route the core asks it to show. Called once per render by `terminal_deck`;
//! measurement goes through `deck_dom`. Ports the effects of
//! `apps/web/src/components/deck/terminal-deck-model.ts`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::{
    DECK_WARM_LIMIT, DeckFolder, DeckIntent, DeckNavigation, DeckSize, WarmSet,
};

use super::deck_dom::{self, SizeWatch};
use crate::pump::Pump;
use crate::router_state::use_navigate;

/// The custom property the desktop strip height resolves from.
const STRIP_HEIGHT_VAR: &str = "--workbench-tab-strip-height";

/// The deck element and what was measured off it.
#[derive(Clone)]
pub struct DeckMeasure {
    /// The mounted deck element.
    pub element: Signal<Option<Rc<MountedData>>>,
    /// Its content box; zero until measured, which paints no pane.
    pub size: Signal<DeckSize>,
    /// `--workbench-tab-strip-height` on the deck; 0 when unset.
    pub desktop_strip_height: Signal<f64>,
    watch: Rc<RefCell<Option<SizeWatch>>>,
}

impl std::fmt::Debug for DeckMeasure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeckMeasure")
            .finish_non_exhaustive()
    }
}

/// The deck's measurement state, empty until [`DeckMeasure::attach`].
pub fn use_deck_measure() -> DeckMeasure {
    DeckMeasure {
        element: use_signal(|| None),
        size: use_signal(DeckSize::default),
        desktop_strip_height: use_signal(|| 0.0),
        watch: use_hook(Rc::default),
    }
}

impl DeckMeasure {
    /// The deck mounted: keep it, measure it now and on every resize.
    pub fn attach(&self, mounted: Rc<MountedData>) {
        let mut element = self.element;
        element.set(Some(Rc::clone(&mounted)));
        let mut size = self.size;
        let mut measure = move || {
            let Ok(current) = element.try_peek() else {
                return;
            };
            let Some((w, h)) = current.as_deref().and_then(deck_dom::client_size) else {
                return;
            };
            drop(current);
            let next = DeckSize { w, h };
            if *size.peek() != next {
                tracing::debug!(target: "deck", w, h, "deck measured");
                size.set(next);
            }
        };
        let watch = SizeWatch::new(measure);
        if let Some(watch) = watch.as_ref() {
            watch.watch(&mounted);
        }
        *self.watch.borrow_mut() = watch;
        measure();
        let mut strip = self.desktop_strip_height;
        strip.set(deck_dom::css_px_var(&mounted, STRIP_HEIGHT_VAR));
    }
}

/// Advance the warm set whenever the open sessions or the slotted ones move;
/// returns the set to render from.
pub fn use_warm_set(open_ids: &[String], slotted_ids: &[String]) -> Signal<WarmSet> {
    let mut warm = use_signal(WarmSet::new);
    use_effect(use_reactive(
        (&open_ids.to_vec(), &slotted_ids.to_vec()),
        move |(open, slotted)| {
            let open: BTreeSet<String> = open.into_iter().collect();
            let mut next = warm.peek().clone();
            if next.advance(&open, &slotted, DECK_WARM_LIMIT) {
                tracing::debug!(target: "deck", warm = next.len(), "deck warm set moved");
                warm.set(next);
            }
        },
    ));
    warm
}

/// What the deck reports to the core each time it moves.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckObservation {
    /// The followed session's folder.
    pub folder: Option<DeckFolder>,
    /// The followed session.
    pub followed_session_id: Option<String>,
    /// Whether the host paints one pane.
    pub compact: bool,
    /// How many panes paint this frame.
    pub visible_pane_count: u32,
}

/// Report the observation to the core whenever it changes.
pub fn use_deck_observation(pump: &Pump, observation: DeckObservation) {
    let pump = pump.clone();
    use_effect(use_reactive((&observation,), move |(observation,)| {
        pump.dispatch(ClientEvent::Deck(DeckIntent::Observed {
            folder: observation.folder,
            followed_session_id: observation.followed_session_id,
            compact: observation.compact,
            visible_pane_count: observation.visible_pane_count,
        }));
    }));
}

/// Show each route an intent asks for, once. A request older than the mount
/// was meant for a deck that no longer exists, so it is not replayed.
pub fn use_deck_navigation(navigation: Option<DeckNavigation>) {
    let navigate = use_navigate();
    let initial = navigation.as_ref().map_or(0, |request| request.sequence);
    let seen: Rc<Cell<u64>> = use_hook(|| Rc::new(Cell::new(initial)));
    use_effect(use_reactive((&navigation,), move |(navigation,)| {
        let Some(request) = navigation else { return };
        if request.sequence <= seen.get() {
            return;
        }
        seen.set(request.sequence);
        tracing::info!(target: "deck", sequence = request.sequence, path = %request.path, "deck navigation followed");
        navigate.call(request.path);
    }));
}
