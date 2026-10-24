//! The terminal key grid: the keys a phone, a D-pad or a TV remote cannot
//! reach, as one ordered list in v2's DOM order, because DOM order is the tab
//! order for anyone walking it with a keyboard and where the controller's
//! first-key focus starts. Drawn by the floating sheet (`terminal_nav_pad`)
//! and by the TV input tray inside the composer bar. Owned Sx styles place the
//! cells by `data-placement`. Ports `TerminalNavButtons.tsx`'s grid.

#[path = "terminal_nav_key_style.rs"]
mod key_style;

use dioxus::prelude::*;
use libero::components::Pictogram;
use libero::components::SvgData;
use roost_client_core::ClientEvent;
use roost_client_core::store::shell_intent::ShellIntent;

use super::pane_handle::PaneHandle;
use super::pane_state::PaneUi;
use crate::pump::Pump;
use libero::components::{Box, Button, Paper, Text};

/// The key grid. `placement` is `sheet` for the floating sheet and `tray` for
/// the TV input tray.
#[component]
pub fn TerminalKeyGrid(
    handle: PaneHandle,
    ui: PaneUi,
    pump: Pump,
    on_ctrl_armed: EventHandler<bool>,
    on_link_armed: EventHandler<bool>,
    placement: &'static str,
) -> Element {
    let revision = pump.revision();
    let _ = revision.read();
    let mouse_forward = pump.core().borrow().store().prefs.mouse_forward;
    let styles = key_style::use_nav_key_styles();
    let cells = grid_cells(
        &handle,
        &pump,
        on_ctrl_armed,
        on_link_armed,
        (ui.ctrl_armed)(),
        (ui.link_armed)(),
        mouse_forward,
    );
    rsx! {
        Paper {
            shadow: "lg",
            sx: styles.paper.clone(),
            "data-testid": "terminal-nav-buttons",
            "data-placement": placement,
            "data-keypad": "true",
            Box {
                component: "div",
                sx: styles.grid.clone(),
                "data-keypad-grid": "true",
                for cell in cells {
                    {key_cell(cell, &styles)}
                }
            }
        }
    }
}

/// Drop the Ctrl and Alt latches whenever the key pad closes. A close is the
/// ONE thing that can drop a latched Ctrl, so the pane reads the store's close
/// counter rather than owning a callback to unregister. Called by the pane,
/// which outlives every open and close of either key surface.
pub fn use_key_latch_disarm(pump: &Pump, ui: PaneUi) {
    let revision = pump.revision();
    let disarm_count = pump.core().borrow().store().terminal_nav_pad.disarm_count();
    let seen = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(disarm_count)));
    let latch_pump = pump.clone();
    // The effect reads the revision itself: a count captured from the render
    // that created it is never the count a later close wrote.
    use_effect(move || {
        let _ = revision.read();
        let current = latch_pump
            .core()
            .borrow()
            .store()
            .terminal_nav_pad
            .disarm_count();
        if seen.get() == current {
            return;
        }
        seen.set(current);
        let mut ctrl_armed = ui.ctrl_armed;
        let mut link_armed = ui.link_armed;
        ctrl_armed.set(false);
        link_armed.set(false);
        tracing::info!(target: "input_nav", "nav pad closed; modifier latches dropped");
    });
}

/// Keep the terminal's focus: every cell here is one tap into a pane the reader
/// must not be pulled out of.
fn keep_focus(event: MouseEvent) {
    event.prevent_default();
}

/// One grid cell. A plain function, not a component: the grid is built per
/// render from one ordered list, so a cell has no lifecycle of its own to
/// preserve and needs no comparable prop.
fn key_cell(cell: NavCell, styles: &key_style::NavKeyStyles) -> Element {
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
            variant: "tonal",
            color: "secondary",
            size: "md",
            sx: styles.key.clone(),
            "data-testid": test_id,
            "data-area": area,
            "data-active": active.then_some("true"),
            selected: latched.map(|_| active),
            "aria-label": aria_label,
            onmousedown: keep_focus,
            onclick: move |event| press.call(event),
            if let Some(icon) = face_icon(face) {
                Text { component: "span", sx: styles.icon.clone(), Pictogram {
                    icon,
                    width: "var(--term-nav-icon-size)",
                    height: "var(--term-nav-icon-size)",
                } }
            }
            if let Some(label) = label {
                Text { component: "span", sx: styles.label.clone(), "{label}" }
            }
        }
    }
}

/// The glyph a cell draws, if it draws one.
fn face_icon(face: KeyFace) -> Option<SvgData> {
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

/// The sixteen cells: v2's fifteen in v2's DOM order, plus Space before Enter.
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
            KeyFace::Icon(pictogram_icons_lucide::delete::outlined),
            "Backspace",
        ),
        key("home", "nav-home", "Home", KeyFace::Label("home"), "Home"),
        key(
            "up",
            "nav-up",
            "Up arrow",
            KeyFace::Icon(pictogram_icons_lucide::chevron_up::outlined),
            "ArrowUp",
        ),
        key("end", "nav-end", "End", KeyFace::Label("end"), "End"),
        key(
            "pgup",
            "nav-pgup",
            "Page up",
            KeyFace::Icon(pictogram_icons_lucide::chevrons_up::outlined),
            "PageUp",
        ),
        key(
            "left",
            "nav-left",
            "Left arrow",
            KeyFace::Icon(pictogram_icons_lucide::chevron_left::outlined),
            "ArrowLeft",
        ),
        key(
            "down",
            "nav-down",
            "Down arrow",
            KeyFace::Icon(pictogram_icons_lucide::chevron_down::outlined),
            "ArrowDown",
        ),
        key(
            "right",
            "nav-right",
            "Right arrow",
            KeyFace::Icon(pictogram_icons_lucide::chevron_right::outlined),
            "ArrowRight",
        ),
        key(
            "pgdn",
            "nav-pgdn",
            "Page down",
            KeyFace::Icon(pictogram_icons_lucide::chevrons_down::outlined),
            "PageDown",
        ),
        NavCell {
            area: "space",
            test_id: "nav-space",
            aria_label: "Space",
            face: KeyFace::Label("space"),
            latched: None,
            press: {
                let handle = handle.clone();
                EventHandler::new(move |_event: MouseEvent| handle.type_text(" "))
            },
        },
        key(
            "enter",
            "nav-enter",
            "Enter",
            KeyFace::Icon(pictogram_icons_lucide::corner_down_left::outlined),
            "Enter",
        ),
        NavCell {
            area: "mouse",
            test_id: "nav-mouse",
            aria_label: "Toggle mouse forwarding",
            face: KeyFace::IconAndLabel(
                pictogram_icons_lucide::mouse::outlined,
                if mouse_forward { "on" } else { "off" },
            ),
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

/// One cell of the grid: its grid area, its test id, its accessible name,
/// its face, and whether it is a latch — which is what makes it carry
/// `data-active` and `aria-pressed` at all.
struct NavCell {
    /// The grid area name the cell's `data-area` carries and the cell class places it by.
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

/// A cell's face: a word, a Lucide glyph, or — for the one cell that states
/// its own value — both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFace {
    /// An uppercased word or a bare glyph.
    Label(&'static str),
    /// A Lucide glyph drawn by `Pictogram`.
    Icon(SvgData),
    /// The mouse toggle: its glyph plus the value it states.
    IconAndLabel(SvgData, &'static str),
}
