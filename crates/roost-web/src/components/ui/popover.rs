//! Click-to-open floating dialog anchored to its trigger. The caller owns `open`; trigger
//! clicks and outside clicks report through `on_open_change`, Escape too unless `on_escape` takes it. Depends on
//! `ui::anchored_panel` (placement, focus), `context_menu` (dismissal) and `ui::field` (ids).

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::context_menu::use_floating_menu_dismiss;
use crate::components::ui::anchored_panel::{anchored_placement_style, focus_element_by_id};
use crate::components::ui::field::scoped_element_id;

/// What the trigger and panel read from the root. Memoised so consumers re-render
/// only when `open` or `test_id` change.
#[derive(Clone, PartialEq)]
struct PopoverView {
    open: bool,
    test_id: Option<String>,
}

#[derive(Clone)]
struct PopoverContext {
    view: Memo<PopoverView>,
    on_open_change: EventHandler<bool>,
    trigger_id: String,
    content_id: String,
    on_escape: Option<EventHandler<()>>,
}

/// Non-modal dialog anchored to a trigger. Nothing here opens or closes the panel
/// on its own; every transition is reported through `on_open_change`.
#[component]
pub fn Popover(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] test_id: Option<String>,
    /// Pinned by tests or by another component's focus logic (`/model` focuses the trigger by id); generated otherwise.
    #[props(into, optional)]
    trigger_id: Option<String>,
    /// Replaces the default Escape (refocus the trigger, then close) for a caller whose Escape steps back through its own state first; the caller then owns closing.
    #[props(optional)]
    on_escape: Option<EventHandler<()>>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let view = use_memo(use_reactive((&open, &test_id), |(open, test_id)| {
        PopoverView { open, test_id }
    }));
    let trigger_id =
        use_hook(|| trigger_id.unwrap_or_else(|| scoped_element_id("popover-trigger")));
    let content_id = use_hook(|| scoped_element_id("popover-content"));
    use_context_provider(|| PopoverContext {
        view,
        on_open_change,
        on_escape,
        trigger_id,
        content_id,
    });
    let root_class = tw_merge!("inline-block", class.as_deref().unwrap_or(""));
    rsx! {
        div { "data-name": "Popover", class: "{root_class}", {children} }
    }
}

/// The button that toggles the panel. It reports `!open` and holds no state of its own.
#[component]
pub fn PopoverTrigger(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] title: Option<String>,
    /// Drops the bordered input look for a control that sits inside a toolbar row.
    #[props(default)]
    ghost: bool,
    #[props(default)] disabled: bool,
    #[props(into, optional)] aria_label: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<PopoverContext>();
    let is_open = (context.view)().open;
    let trigger_id = context.trigger_id.clone();
    let content_id = context.content_id.clone();
    let on_open_change = context.on_open_change;
    let surface = if ghost {
        "hover:bg-accent hover:text-accent-foreground"
    } else {
        "border bg-background border-input hover:bg-accent hover:text-accent-foreground"
    };
    let trigger_class = tw_merge!(
        "px-4 py-2 h-9 inline-flex justify-center items-center text-sm font-medium whitespace-nowrap rounded-md transition-colors w-fit focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 [&_svg:not(:last-child)]:mr-2 [&_svg:not(:first-child)]:ml-2",
        surface,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            id: "{trigger_id}",
            r#type: "button",
            "data-name": "PopoverTrigger",
            class: "{trigger_class}",
            "data-testid": test_id,
            title,
            disabled,
            "aria-label": aria_label.as_deref(),
            "aria-haspopup": "dialog",
            "aria-expanded": if is_open { "true" } else { "false" },
            "aria-controls": is_open.then_some(content_id),
            onclick: move |_| on_open_change.call(!is_open),
            {children}
        }
    }
}

/// The floating dialog. It is absent from the DOM while closed, so nothing inside
/// it is focusable or announced.
#[component]
pub fn PopoverContent(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let context = use_context::<PopoverContext>();
    let view = (context.view)();
    if !view.open {
        return rsx! {};
    }
    rsx! {
        PopoverPanel { class, test_id: view.test_id, {children} }
    }
}

#[component]
fn PopoverPanel(class: Option<String>, test_id: Option<String>, children: Element) -> Element {
    let context = use_context::<PopoverContext>();
    let trigger_id = context.trigger_id.clone();
    let content_id = context.content_id.clone();
    let on_open_change = context.on_open_change;
    let escape_trigger_id = trigger_id.clone();
    // Hooks capture their handlers once at mount, so the callbacks must be stable
    // handles; `on_open_change` is updated in place when the root re-renders.
    let on_close = use_callback(move |()| on_open_change.call(false));
    let caller_escape = context.on_escape;
    let on_escape = use_callback(move |()| match caller_escape {
        Some(handler) => handler.call(()),
        None => {
            focus_element_by_id(&escape_trigger_id);
            on_open_change.call(false);
        }
    });
    use_floating_menu_dismiss(
        on_close,
        Some(on_escape),
        vec![trigger_id.clone(), content_id.clone()],
    );
    let focus_content_id = content_id.clone();
    use_effect(move || focus_element_by_id(&focus_content_id));
    use_hook(|| tracing::debug!(target: "ui", "popover opened"));
    use_drop(|| tracing::debug!(target: "ui", "popover closed"));
    let style = anchored_placement_style(&trigger_id, false).unwrap_or_default();
    let panel_class = tw_merge!(
        "overflow-visible fixed z-50 p-4 rounded-md border bg-popover text-popover-foreground shadow-md w-64 outline-hidden",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            id: "{content_id}",
            role: "dialog",
            tabindex: "-1",
            "data-name": "PopoverContent",
            "data-testid": test_id.as_deref(),
            class: "{panel_class}",
            style: "{style}",
            {children}
        }
    }
}

#[component]
pub fn PopoverTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "leading-none font-medium mb-3",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        h3 { "data-name": "PopoverTitle", class: "{merged}", {children} }
    }
}

#[component]
pub fn PopoverDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-muted-foreground text-sm",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        p { "data-name": "PopoverDescription", class: "{merged}", {children} }
    }
}
