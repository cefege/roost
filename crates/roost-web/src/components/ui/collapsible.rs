//! A disclosure: a trigger that flips a parent-owned `open` flag, and content
//! that stays mounted and animates its grid rows between `0fr` and `1fr`. The
//! `data-state` attribute on each part carries the open state, so CSS drives the
//! reveal. Not an overlay: no dismiss, focus trap or scrim.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::button::{Button, ButtonSize, ButtonVariant};

#[derive(Clone, Copy)]
struct CollapsibleContext {
    open: bool,
    on_open_change: EventHandler<bool>,
}

fn state_label(open: bool) -> &'static str {
    if open { "open" } else { "closed" }
}

#[component]
pub fn Collapsible(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    // Children read the context with `consume_context` on every render:
    // `use_context` caches the first value, so a later `open` would never reach them.
    provide_context(CollapsibleContext {
        open,
        on_open_change,
    });
    let state = state_label(open);
    let merged = tw_merge!("", class.as_deref().unwrap_or(""));

    rsx! {
        div {
            "data-name": "Collapsible",
            "data-state": "{state}",
            class: "{merged}",
            {children}
        }
    }
}

/// A `Button` that toggles the enclosing `Collapsible`. `controls_id` names the
/// content it reveals; `test_id` names it for tests; `disabled` holds it shut
/// while the content must stay closed.
#[component]
pub fn CollapsibleTrigger(
    #[props(default = ButtonVariant::Ghost)] variant: ButtonVariant,
    #[props(default = ButtonSize::Default)] size: ButtonSize,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default)] disabled: bool,
    #[props(into, optional)] controls_id: Option<String>,
    children: Element,
) -> Element {
    let CollapsibleContext {
        open,
        on_open_change,
    } = consume_context::<CollapsibleContext>();
    let state = state_label(open);

    rsx! {
        Button {
            variant,
            size,
            class,
            disabled,
            "data-name": "CollapsibleTrigger",
            "data-state": "{state}",
            "aria-expanded": if open { "true" } else { "false" },
            "data-testid": test_id.as_deref(),
            "aria-controls": controls_id.as_deref(),
            onclick: move |_| on_open_change.call(!open),
            {children}
        }
    }
}

/// The content of a `Collapsible`. It stays mounted while closed and collapses
/// to zero rows, so the reveal animates.
#[component]
pub fn CollapsibleContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] outer_class: Option<String>,
    children: Element,
) -> Element {
    let CollapsibleContext { open, .. } = consume_context::<CollapsibleContext>();
    let state = state_label(open);

    let outer = tw_merge!(
        "grid overflow-hidden transition-all duration-300 data-[state=closed]:grid-rows-[0fr] data-[state=open]:grid-rows-[1fr]",
        outer_class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            "data-name": "CollapsibleContent",
            "data-state": "{state}",
            class: "{outer}",
            // Collapsed content stays mounted for rust-ui's height transition;
            // `inert` keeps its controls out of the tab order and the AT tree.
            div { class: tw_merge!("min-h-0", class.as_deref().unwrap_or("")), inert: !open, {children} }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use dioxus::core::NoOpMutations;
    use dioxus::prelude::*;

    use super::{Collapsible, CollapsibleContent, CollapsibleContext};

    thread_local! {
        static OPEN_HANDLE: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
        static TRIGGER_SEEN: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
        static CONTENT_MOUNTS: Cell<usize> = const { Cell::new(0) };
    }

    #[component]
    fn ProbeTrigger(children: Element) -> Element {
        let CollapsibleContext { open, .. } = consume_context::<CollapsibleContext>();
        TRIGGER_SEEN.with(|seen| seen.borrow_mut().push(open));
        rsx! { {children} }
    }

    #[component]
    fn ProbeContent() -> Element {
        use_hook(|| CONTENT_MOUNTS.with(|mounts| mounts.set(mounts.get() + 1)));
        rsx! {}
    }

    fn toggled_root() -> Element {
        let mut open = use_hook(|| Signal::new(false));
        OPEN_HANDLE.with(|slot| *slot.borrow_mut() = Some(open));
        rsx! {
            Collapsible {
                open: open(),
                on_open_change: move |next: bool| open.set(next),
                ProbeTrigger { span {} }
                CollapsibleContent { ProbeContent {} }
            }
        }
    }

    fn settle(dom: &mut VirtualDom) {
        for _ in 0..4 {
            dom.render_immediate(&mut NoOpMutations);
            dom.process_events();
        }
    }

    fn mounted() -> (VirtualDom, Signal<bool>) {
        let mut dom = VirtualDom::new(toggled_root);
        dom.rebuild_in_place();
        settle(&mut dom);
        let Some(open) = OPEN_HANDLE.with(|slot| *slot.borrow()) else {
            panic!("the root component runs during the first rebuild and publishes its signal");
        };
        (dom, open)
    }

    fn toggle_to(dom: &mut VirtualDom, open: &mut Signal<bool>, next: bool) {
        dom.in_runtime(|| open.set(next));
        settle(dom);
    }

    fn last_trigger_seen() -> Option<bool> {
        TRIGGER_SEEN.with(|seen| seen.borrow().last().copied())
    }

    #[test]
    fn trigger_follows_every_toggle_and_content_stays_mounted() {
        TRIGGER_SEEN.with(|seen| seen.borrow_mut().clear());
        CONTENT_MOUNTS.with(|mounts| mounts.set(0));
        let (mut dom, mut open) = mounted();
        assert_eq!(last_trigger_seen(), Some(false));
        assert_eq!(CONTENT_MOUNTS.with(|mounts| mounts.get()), 1);

        toggle_to(&mut dom, &mut open, true);
        assert_eq!(last_trigger_seen(), Some(true));
        assert_eq!(CONTENT_MOUNTS.with(|mounts| mounts.get()), 1);

        toggle_to(&mut dom, &mut open, false);
        assert_eq!(last_trigger_seen(), Some(false));
        assert_eq!(CONTENT_MOUNTS.with(|mounts| mounts.get()), 1);
    }
}
