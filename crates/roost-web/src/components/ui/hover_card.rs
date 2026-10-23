//! Hover-revealed floating card anchored to its trigger. The caller owns `open`; pointer and
//! focus events on the trigger and card report through `on_open_change`. A close waits out a
//! grace period and runs only once neither the pointer nor the focus is inside either surface.
//! Depends on `ui::anchored_panel`, `context_menu` (Escape, outside click) and `deck::deck_dom::Timeout`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::context_menu::use_floating_menu_dismiss;
use crate::components::deck::deck_dom::Timeout;
use crate::components::ui::anchored_panel::anchored_placement_style;
use crate::components::ui::field::scoped_element_id;

/// Grace period before a leave or blur closes the card. Without it the trigger-to-card
/// gap closes the card before the pointer can reach it.
const HOVER_CARD_CLOSE_DELAY_MS: u32 = 150;

/// Edge of the trigger the card opens beside. Left and right are not offered: the shared
/// placement arithmetic positions vertically only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HoverCardSide {
    Top,
    #[default]
    Bottom,
}

#[derive(Clone, PartialEq)]
struct HoverCardView {
    open: bool,
    test_id: Option<String>,
    side: HoverCardSide,
}

/// Which input is over the trigger or card. Each is tracked separately so that a focus
/// move inside the card does not close it while the pointer still rests on it, and the
/// reverse.
#[derive(Clone, Copy)]
enum Presence {
    Pointer,
    Focus,
}

#[derive(Default)]
struct HoverState {
    pointer_inside: bool,
    focus_inside: bool,
    pending_close: Option<Timeout>,
}

#[derive(Clone)]
struct HoverCardContext {
    view: Memo<HoverCardView>,
    on_open_change: EventHandler<bool>,
    trigger_id: String,
    content_id: String,
    hover: Rc<RefCell<HoverState>>,
}

/// Hover card whose visibility is entirely the caller's `open` state.
#[component]
pub fn HoverCard(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default)] side: HoverCardSide,
    children: Element,
) -> Element {
    let view = use_memo(use_reactive(
        (&open, &test_id, &side),
        |(open, test_id, side)| HoverCardView {
            open,
            test_id,
            side,
        },
    ));
    let trigger_id = use_hook(|| scoped_element_id("hover-card-trigger"));
    let content_id = use_hook(|| scoped_element_id("hover-card-content"));
    let hover = use_hook(|| Rc::new(RefCell::new(HoverState::default())));
    use_context_provider(|| HoverCardContext {
        view,
        on_open_change,
        trigger_id,
        content_id,
        hover,
    });
    rsx! {
        {children}
    }
}

/// The element that reveals the card on pointer enter or focus.
#[component]
pub fn HoverCardTrigger(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<HoverCardContext>();
    let is_open = (context.view)().open;
    let trigger_id = context.trigger_id.clone();
    let content_id = context.content_id.clone();
    let on_open_change = context.on_open_change;
    let hover_enter = context.hover.clone();
    let hover_leave = context.hover.clone();
    let hover_focus = context.hover.clone();
    let hover_blur = context.hover.clone();
    let merged = tw_merge!("inline-block", class.as_deref().unwrap_or(""));
    rsx! {
        span {
            id: "{trigger_id}",
            "data-name": "HoverCardTrigger",
            class: "{merged}",
            tabindex: "0",
            "aria-describedby": is_open.then_some(content_id),
            onmouseenter: move |_| set_presence(&hover_enter, Presence::Pointer, true, on_open_change),
            onmouseleave: move |_| set_presence(&hover_leave, Presence::Pointer, false, on_open_change),
            onfocus: move |_| set_presence(&hover_focus, Presence::Focus, true, on_open_change),
            onblur: move |_| set_presence(&hover_blur, Presence::Focus, false, on_open_change),
            {children}
        }
    }
}

/// The floating card. Absent from the DOM while closed.
#[component]
pub fn HoverCardContent(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<HoverCardContext>();
    let view = (context.view)();
    if !view.open {
        return rsx! {};
    }
    rsx! {
        HoverCardPanel { class, test_id: view.test_id, side: view.side, {children} }
    }
}

#[component]
fn HoverCardPanel(
    class: Option<String>,
    test_id: Option<String>,
    side: HoverCardSide,
    children: Element,
) -> Element {
    let context = use_context::<HoverCardContext>();
    let trigger_id = context.trigger_id.clone();
    let content_id = context.content_id.clone();
    let on_open_change = context.on_open_change;
    let hover_enter = context.hover.clone();
    let hover_leave = context.hover.clone();
    let hover_focus_in = context.hover.clone();
    let hover_focus_out = context.hover.clone();
    // Stable handle, as in `PopoverPanel`: the dismissal hook captures it once at mount.
    let on_close = use_callback(move |()| on_open_change.call(false));
    use_floating_menu_dismiss(
        on_close,
        Some(on_close),
        vec![trigger_id.clone(), content_id.clone()],
    );
    use_hook(|| tracing::debug!(target: "ui", "hover card opened"));
    use_drop(|| tracing::debug!(target: "ui", "hover card closed"));
    // Placement is measured from the trigger; `position: fixed` rides in the inline style so the
    // class list stays rust-ui's.
    let placement = anchored_placement_style(&trigger_id, side == HoverCardSide::Top, false)
        .unwrap_or_default();
    let style = format!("position: fixed; {placement}");
    let card_class = tw_merge!(
        "overflow-visible relative z-50 p-4 rounded-lg border bg-card shadow-md w-64",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            id: "{content_id}",
            role: "dialog",
            "data-name": "HoverCardContent",
            "data-testid": test_id.as_deref(),
            class: "{card_class}",
            style: "{style}",
            onmouseenter: move |_| set_presence(&hover_enter, Presence::Pointer, true, on_open_change),
            onmouseleave: move |_| set_presence(&hover_leave, Presence::Pointer, false, on_open_change),
            onfocusin: move |_| set_presence(&hover_focus_in, Presence::Focus, true, on_open_change),
            onfocusout: move |_| set_presence(&hover_focus_out, Presence::Focus, false, on_open_change),
            {children}
        }
    }
}

/// Records whether the pointer or focus is inside the trigger or card. Entering cancels any
/// pending close. Leaving schedules one only when neither input remains inside, so a
/// `focusout` while the pointer rests on the card does not close it.
fn set_presence(
    hover: &RefCell<HoverState>,
    presence: Presence,
    inside: bool,
    on_open_change: EventHandler<bool>,
) {
    let mut state = hover.borrow_mut();
    match presence {
        Presence::Pointer => state.pointer_inside = inside,
        Presence::Focus => state.focus_inside = inside,
    }
    if inside {
        // Dropping the cancelled timer is what stops a pending close.
        state.pending_close.take();
        drop(state);
        on_open_change.call(true);
    } else if !state.pointer_inside && !state.focus_inside {
        state.pending_close = Timeout::after(HOVER_CARD_CLOSE_DELAY_MS, move || {
            on_open_change.call(false)
        });
    }
}
