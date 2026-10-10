//! The inline geometry the workbench shell writes: the grid's height and
//! sidebar-width properties, the editor region's keyboard shift and composer
//! reservation, the `--roost-main-left` offset, when the compact top bar
//! shows, and where the compact drawer stands on each route. Ports
//! `shellStyle`, `editorStyle` and the memos of
//! `apps/web/src/components/layout/AppShell.tsx`; read by `AppShell`.
//!
//! A compact terminal route reserves the composer's RESTING row whether or not
//! the composer is mounted, and the soft keyboard only translates the region:
//! a PTY resize makes an inline TUI repaint, and one repainting in place
//! duplicates rows into history. `keyboard_resize` is the explicit opt-in.

use roost_client_core::store::sidebar::SidebarIntent;

use crate::routes::Route;

/// What the composer is doing, when a composer is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ComposerGeometry {
    /// Whether the composer is open and taking input.
    ///
    /// `Default` is "no composer": the shell that owns the reactive signal is
    /// created before any dock has mounted, and an inactive composer reserving
    /// a measured height would be a reserve for a composer that is not there.
    pub active: bool,
    /// Its measured height in pixels.
    pub height_px: f64,
}

/// The shell's inline style: height, then the three sidebar-width properties.
pub fn shell_style(
    keyboard_resize: bool,
    composer_active: bool,
    sidebar_width_px: u32,
    collapsed: bool,
) -> String {
    let height = if keyboard_resize && !composer_active {
        "calc(100svh - var(--kb-offset))"
    } else {
        "100svh"
    };
    let (width, resizer) = if collapsed {
        ("0px", "0px")
    } else {
        (
            "var(--workbench-sidebar-expanded-width)",
            "var(--workbench-sidebar-resizer-width)",
        )
    };
    format!(
        "height: {height}; --workbench-sidebar-expanded-width: {sidebar_width_px}px; \
         --workbench-sidebar-width: {width}; --workbench-sidebar-resizer-active-width: {resizer};"
    )
}

/// The editor region's inline style.
///
/// Every branch names every property it can ever set. Dioxus MERGES a `style`
/// update: a property the new value omits keeps its previous value, so a
/// padding written by one early compact render used to outlive the switch to
/// the desktop layout and leave an empty band under the deck.
pub fn editor_style(
    terminal_route: bool,
    reserves_composer: bool,
    compact: bool,
    keyboard_resize: bool,
    composer: ComposerGeometry,
) -> String {
    let shifts = terminal_route && !keyboard_resize;
    let transform = if shifts {
        "translateY(calc(var(--kb-offset) * -1))"
    } else {
        "none"
    };
    let reserve = shifts && compact && reserves_composer;
    let padding = if reserve {
        "calc(var(--term-chat-rest-height) + var(--term-chat-dock-rest-offset))"
    } else {
        "0px"
    };
    let growth = if reserve && composer.active {
        format!(
            "max(0px, calc({}px - var(--term-chat-rest-height)))",
            composer.height_px
        )
    } else {
        "0px".to_owned()
    };
    format!("--term-chat-growth: {growth}; transform: {transform}; padding-bottom: {padding};")
}

/// `data-composer-reserve`: the compact editor keeps the portaled terminal
/// composer's resting row. An agent tab carries its composer inline in the
/// deck slot, so it shifts for the keyboard but reserves nothing.
#[must_use]
pub fn reserves_terminal_composer(pathname: &str, keyboard_resize: bool) -> bool {
    keyboard_shift(is_terminal_path(pathname), keyboard_resize) && !pathname.starts_with("/a/")
}

/// `data-keyboard-shift`: set on a terminal route unless the reader opted into
/// keyboard resizing.
///
/// The soft keyboard TRANSLATES the editor region and never resizes it, and
/// that has to hold while the composer is active — the active composer is the
/// reason the keyboard is open. Gating this on the composer's state took the
/// translation away the moment the soft keyboard appeared, so the terminal
/// dropped back down under the raised dock while the dock's own offset still
/// counted the inset. The reserve and the shift are ONE decision: both belong
/// to this predicate.
#[must_use]
pub fn keyboard_shift(terminal_route: bool, keyboard_resize: bool) -> bool {
    terminal_route && !keyboard_resize
}

/// `--roost-main-left`: where the editor starts, for fixed overlays that align
/// to it.
pub fn main_left_offset(compact: bool, collapsed: bool, sidebar_width_px: u32) -> String {
    if compact {
        return "0px".to_owned();
    }
    let sidebar = if collapsed {
        "0px".to_owned()
    } else {
        format!("calc({sidebar_width_px}px + var(--workbench-sidebar-resizer-width))")
    };
    format!("calc(var(--workbench-activity-width) + {sidebar})")
}

/// The compact top bar shows off the root, browse, settings and terminal routes
/// (those carry their own chrome).
pub fn shows_mobile_top_bar(compact: bool, pathname: &str, terminal_route: bool) -> bool {
    compact
        && pathname != "/"
        && !pathname.starts_with("/browse")
        && !pathname.starts_with("/settings")
        && !terminal_route
}

/// v2's terminal-route test for the shell: a prefix, so a terminal route that
/// has not resolved yet still shifts for the keyboard.
pub fn is_terminal_path(pathname: &str) -> bool {
    pathname.starts_with("/s/")
        || pathname.starts_with("/t/")
        || pathname.starts_with("/w/")
        || pathname.starts_with("/a/")
}

/// Where the drawer stands when the route or the size class changes.
///
/// A compact home opens it: the home landing names no session, and on a TV
/// remote with no swipe it would be a dead end, so the session list IS the
/// compact home. Every other route closes it, and so does the desktop layout,
/// where a drawer left open by a narrower window would still hide the
/// portaled composer.
pub fn drawer_intent_for_route(compact: bool, pathname: &str) -> SidebarIntent {
    if compact && Route::parse(pathname) == Route::Home {
        SidebarIntent::OpenDrawer
    } else {
        SidebarIntent::CloseDrawer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compact_home_opens_on_the_session_list() {
        for pathname in ["/", "", "/?from=pair", "/#top"] {
            assert_eq!(
                drawer_intent_for_route(true, pathname),
                SidebarIntent::OpenDrawer,
                "{pathname}"
            );
        }
    }

    #[test]
    fn a_compact_route_that_shows_something_closes_the_drawer() {
        for pathname in [
            "/s/abc",
            "/t/fp/src",
            "/w/ws1",
            "/browse",
            "/settings",
            "/help",
        ] {
            assert_eq!(
                drawer_intent_for_route(true, pathname),
                SidebarIntent::CloseDrawer,
                "{pathname}"
            );
        }
    }

    #[test]
    fn the_desktop_home_never_opens_the_drawer() {
        assert_eq!(
            drawer_intent_for_route(false, "/"),
            SidebarIntent::CloseDrawer
        );
    }
}

#[cfg(test)]
mod editor_style_tests {
    use super::*;

    fn idle() -> ComposerGeometry {
        ComposerGeometry::default()
    }

    #[test]
    fn every_editor_style_names_every_property_it_can_set() {
        for (terminal, reserves, compact, resize) in [
            (false, false, false, false),
            (true, true, false, false),
            (true, true, true, false),
            (true, false, true, false),
            (true, true, true, true),
        ] {
            let style = editor_style(terminal, reserves, compact, resize, idle());
            for property in ["--term-chat-growth:", "transform:", "padding-bottom:"] {
                assert!(style.contains(property), "{property} missing from {style}");
            }
        }
    }

    #[test]
    fn only_a_compact_terminal_route_reserves_the_composer_row() {
        assert!(
            editor_style(true, true, true, false, idle()).contains("var(--term-chat-rest-height)")
        );
        assert!(editor_style(true, true, false, false, idle()).contains("padding-bottom: 0px"));
        assert!(editor_style(true, false, true, false, idle()).contains("padding-bottom: 0px"));
    }

    #[test]
    fn an_agent_tab_shifts_for_the_keyboard_but_reserves_nothing() {
        assert!(keyboard_shift(is_terminal_path("/a/7"), false));
        assert!(!reserves_terminal_composer("/a/7", false));
        assert!(reserves_terminal_composer("/s/abc", false));
        assert!(!reserves_terminal_composer("/s/abc", true));
    }
}
