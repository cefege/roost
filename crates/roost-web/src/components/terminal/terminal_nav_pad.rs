//! The touch/controller terminal key sheet: the keys a phone or a D-pad cannot
//! reach, plus the fixed toggle that opens it.
//!
//! Renders as body-level fixed overlays, OUTSIDE the composer dock's subtree,
//! so opening it moves neither the dock nor the terminal. The grid is one
//! ordered list in v2's DOM order, because DOM order is the tab order for
//! anyone walking the sheet with a keyboard and where the controller's
//! first-key focus starts. Ports
//! `apps/web/src/components/terminal/TerminalNavButtons.tsx`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::terminal_nav_pad::terminal_nav_pad_open;

use super::pane_handle::PaneHandle;
use super::pane_state::PaneUi;
use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize};
use crate::pump::Pump;

/// One cell of the grid: its stylesheet area, its test id, its accessible name,
/// its face, and whether it is a latch — which is what makes it carry
/// `data-active` and `aria-pressed` at all.
struct NavCell {
    /// The `term-nav__key--*` suffix the stylesheet places the cell by.
    area: &'static str,
    /// The oracle's `data-testid`.
    test_id: &'static str,
    /// The cell's accessible name.
    aria_label: &'static str,
    /// What it shows.
    face: KeyFace,
    /// Its latch state, or `None` for a momentary key.
    latched: Option<bool>,
    /// What one press does.
    press: EventHandler<MouseEvent>,
}

/// A cell's face: a word, a Material ligature, or — for the one cell that states
/// its own value — both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFace {
    /// An uppercased word or a bare glyph.
    Label(&'static str),
    /// A Material Symbols ligature, drawn by `icon.css`.
    Icon(&'static str),
    /// The mouse toggle: its glyph plus the value it states.
    IconAndLabel(&'static str, &'static str),
}

#[component]
pub fn TerminalNavPad(
    handle: PaneHandle,
    ui: PaneUi,
    pump: Pump,
    on_ctrl_armed: EventHandler<bool>,
    on_link_armed: EventHandler<bool>,
) -> Element {
    let revision = pump.revision();
    let _ = revision.read();
    let (open, mouse_forward) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        (terminal_nav_pad_open(store), store.prefs.mouse_forward)
    };
    let ctrl_armed = ui.ctrl_armed;
    let link_armed = ui.link_armed;
    let ctrl_on = (ui.ctrl_armed)();
    let alt_on = (ui.link_armed)();

    // A close is the ONE thing that can drop a latched Ctrl, so the sheet reads
    // the store's close counter rather than owning a callback to unregister.
    let disarm_count = {
        let core = pump.core();
        let core = core.borrow();
        core.store().terminal_nav_pad.disarm_count()
    };
    {
        let seen = use_hook(|| std::cell::Cell::new(disarm_count));
        use_effect(move || {
            if seen.get() == disarm_count {
                return;
            }
            seen.set(disarm_count);
            let mut ctrl_armed = ctrl_armed;
            let mut link_armed = link_armed;
            ctrl_armed.set(false);
            link_armed.set(false);
            tracing::info!(target: "input_nav", "nav pad closed; modifier latches dropped");
        });
    }

    let cells = grid_cells(
        &handle,
        &pump,
        on_ctrl_armed,
        on_link_armed,
        ctrl_on,
        alt_on,
        mouse_forward,
    );
    let toggle_pump = pump.clone();
    let toggle = move |_event: MouseEvent| {
        toggle_pump.dispatch(ClientEvent::Shell(ShellIntent::ToggleNavPad));
    };

    rsx! {
        if open {
            div {
                class: "term-nav",
                "data-testid": "terminal-nav-buttons",
                div {
                    class: "term-nav__grid",
                    for cell in cells {
                        {key_cell(cell)}
                    }
                }
            }
        }
        IconButton {
            icon: toggle_icon(open),
            label: toggle_label(open),
            variant: ButtonVariant::Ghost,
            size: IconButtonSize::IconLg,
            class: Some("term-nav-toggle".to_owned()),
            "data-testid": "terminal-nav-toggle",
            "data-open": if open { "true" } else { "false" },
            onmousedown: keep_focus,
            onclick: toggle,
        }
    }
}

/// The fifteen cells, in v2's DOM order.
fn grid_cells(
    handle: &PaneHandle,
    pump: &Pump,
    on_ctrl_armed: EventHandler<bool>,
    on_link_armed: EventHandler<bool>,
    ctrl_on: bool,
    alt_on: bool,
    mouse_forward: bool,
) -> Vec<NavCell> {
    let key = |area: &'static str,
               test_id: &'static str,
               aria_label: &'static str,
               face: KeyFace,
               dom_key: &'static str| {
        let handle = handle.clone();
        NavCell {
            area,
            test_id,
            aria_label,
            face,
            latched: None,
            press: EventHandler::new(move |_event: MouseEvent| handle.dispatch_key(dom_key)),
        }
    };
    vec![
        key("esc", "nav-esc", "Escape", KeyFace::Label("esc"), "Escape"),
        key("tab", "nav-tab", "Tab", KeyFace::Label("tab"), "Tab"),
        NavCell {
            area: "ctrl",
            test_id: "nav-ctrl",
            aria_label: "Control",
            face: KeyFace::Label("ctrl"),
            latched: Some(ctrl_on),
            // The sheet REPORTS the change; the pane decides what arming means,
            // because only it knows whether this device has a pointer that can
            // focus the terminal on the way.
            press: EventHandler::new(move |_event: MouseEvent| on_ctrl_armed.call(!ctrl_on)),
        },
        NavCell {
            area: "alt",
            test_id: "nav-alt",
            aria_label: "Toggle Alt link activation",
            face: KeyFace::Label("alt"),
            latched: Some(alt_on),
            press: EventHandler::new(move |_event: MouseEvent| on_link_armed.call(!alt_on)),
        },
        key(
            "back",
            "nav-backspace",
            "Backspace",
            KeyFace::Icon("backspace"),
            "Backspace",
        ),
        key("home", "nav-home", "Home", KeyFace::Label("home"), "Home"),
        key(
            "up",
            "nav-up",
            "Up arrow",
            KeyFace::Icon("keyboard_arrow_up"),
            "ArrowUp",
        ),
        key("end", "nav-end", "End", KeyFace::Label("end"), "End"),
        key(
            "pgup",
            "nav-pgup",
            "Page up",
            KeyFace::Icon("keyboard_double_arrow_up"),
            "PageUp",
        ),
        key(
            "left",
            "nav-left",
            "Left arrow",
            KeyFace::Icon("keyboard_arrow_left"),
            "ArrowLeft",
        ),
        key(
            "down",
            "nav-down",
            "Down arrow",
            KeyFace::Icon("keyboard_arrow_down"),
            "ArrowDown",
        ),
        key(
            "right",
            "nav-right",
            "Right arrow",
            KeyFace::Icon("keyboard_arrow_right"),
            "ArrowRight",
        ),
        key(
            "pgdn",
            "nav-pgdn",
            "Page down",
            KeyFace::Icon("keyboard_double_arrow_down"),
            "PageDown",
        ),
        key(
            "enter",
            "nav-enter",
            "Enter",
            KeyFace::Icon("keyboard_return"),
            "Enter",
        ),
        NavCell {
            area: "mouse",
            test_id: "nav-mouse",
            aria_label: "Toggle mouse forwarding",
            face: KeyFace::IconAndLabel("mouse", if mouse_forward { "on" } else { "off" }),
            latched: Some(mouse_forward),
            press: {
                let pump = pump.clone();
                EventHandler::new(move |_event: MouseEvent| {
                    pump.dispatch(ClientEvent::Shell(ShellIntent::SetMouseForward {
                        on: !mouse_forward,
                    }));
                })
            },
        },
    ]
}

/// The toggle's glyph: a chevron while the sheet is up, the keyboard while it
/// is not, so the control states what one press will do.
fn toggle_icon(open: bool) -> String {
    (if open {
        "keyboard_arrow_down"
    } else {
        "keyboard"
    })
    .to_owned()
}

/// The toggle's accessible name.
fn toggle_label(open: bool) -> String {
    (if open {
        "Hide terminal keys"
    } else {
        "Show terminal keys"
    })
    .to_owned()
}

/// Keep the terminal's focus: every cell here is one tap into a pane the reader
/// must not be pulled out of.
fn keep_focus(event: MouseEvent) {
    event.prevent_default();
}

/// One grid cell. A plain function, not a component: the grid is built per
/// render from one ordered list, so a cell has no lifecycle of its own to
/// preserve and needs no comparable prop.
fn key_cell(cell: NavCell) -> Element {
    let NavCell {
        area,
        test_id,
        aria_label,
        face,
        latched,
        press,
    } = cell;
    let active = latched.is_some_and(|on| on);
    let label = face_label(face);
    rsx! {
        Button {
            variant: ButtonVariant::Secondary,
            size: ButtonSize::Icon,
            class: Some(format!("term-nav__key term-nav__key--{area}")),
            "data-testid": test_id,
            "data-active": active.then_some("true"),
            "aria-label": aria_label,
            attributes: latched.map(pressed_attribute).unwrap_or_default(),
            onmousedown: keep_focus,
            onclick: move |event| press.call(event),
            if let Some(icon) = face_icon(face) {
                Icon { name: icon.to_owned(), class: Some("term-nav__icon".to_owned()) }
            }
            if let Some(label) = label {
                span { class: "term-nav__label", "{label}" }
            }
        }
    }
}

/// The ligature a cell draws, if it draws one.
fn face_icon(face: KeyFace) -> Option<&'static str> {
    match face {
        KeyFace::Icon(icon) | KeyFace::IconAndLabel(icon, _) => Some(icon),
        KeyFace::Label(_) => None,
    }
}

/// The word a cell shows, if it shows one.
fn face_label(face: KeyFace) -> Option<&'static str> {
    match face {
        KeyFace::Label(label) => Some(label),
        KeyFace::IconAndLabel(_, label) => Some(label),
        KeyFace::Icon(_) => None,
    }
}

/// `aria-pressed` for a latch cell. A momentary key carries none, so a screen
/// reader does not announce Esc or Enter as a pressed toggle.
fn pressed_attribute(on: bool) -> Vec<Attribute> {
    vec![Attribute::new(
        "aria-pressed",
        if on { "true" } else { "false" },
        None,
        false,
    )]
}
