//! The canonical application shell: the desktop workbench grid (title, rail,
//! sidebar region, editor, status bar) and the compact layout (top bar,
//! drawer). Ported from `apps/web/src/components/layout/AppShell.tsx`; mounted
//! by `app::AuthorizedShell` around every route inside the shell. The route
//! content (MainPane, HomeLanding, …) arrives as `children` in the editor slot;
//! the sidebar surface mounts ONCE — in the desktop region or in the compact
//! drawer, never both.
//!
//! Geometry decisions are `shell_style` and `shell_metrics`; the listeners
//! (sidebar-toggle chord, page-show reset, `--roost-main-left`) are
//! `app_shell_dom`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::Store;
use roost_client_core::store::Session;
use roost_client_core::store::sidebar::SidebarIntent;

use super::activity_bar::ActivityBar;
use super::mobile_bar::MobileTopBar;
use super::mobile_sidebar_drawer::MobileSidebarDrawer;
use super::shell_style::{
    ComposerGeometry, drawer_intent_for_route, editor_style, is_terminal_path, keyboard_shift,
    shell_style, shows_mobile_top_bar,
};
use super::sidebar_region::SidebarRegion;
use super::status_bar::StatusBar;
use super::title_bar::TitleBar;
use super::window_size::use_is_compact;
use crate::components::sidebar::sidebar_root::SidebarRoot;
use crate::components::terminal_chrome::composer_geometry::published_geometry;
use crate::platform::worker_paths::{BrowserWorkerPaths, worker_path_basename};
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::terminal_href::worker_os;

/// What the composer is doing, read from the slot the portaled dock owns.
///
/// NOT a constant, and that is the whole point. A compact terminal route
/// reserves the composer's RESTING row whether or not the composer is mounted
/// right now — it unmounts under the drawer — so the resting reservation is
/// unconditional, and only the measured growth rides on the live value.
/// `docs/FAILURE-INDEX.md`, "Transient chrome resizes the PTY", is the
/// authority: nothing here may change a PTY's row count.
fn composer_geometry() -> ComposerGeometry {
    published_geometry()
}

/// What the chrome shows about the session a path addresses: the OSC title
/// the worker observed, and the basename of the session's live folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChrome {
    /// The terminal's own (OSC) title, when it has one.
    pub title: Option<String>,
    /// The basename of the folder it runs in.
    pub folder: Option<String>,
}

/// v2 `workbenchTitle`'s session half: the OSC title, else the cwd basename.
pub fn session_chrome(store: &Store, session: &Session) -> SessionChrome {
    SessionChrome {
        title: store.terminal_titles.get(session.id.as_str()).cloned(),
        folder: worker_path_basename(worker_os(store, session.worker_fp.as_str()), &session.cwd),
    }
}

/// The shell's store reading, taken in one borrow per render.
#[derive(Debug, Clone, PartialEq)]
struct ShellReading {
    sidebar_width: u32,
    collapsed: bool,
    keyboard_resize: bool,
    chrome: Option<SessionChrome>,
}

fn read_shell(store: &Store, path: &str) -> ShellReading {
    ShellReading {
        sidebar_width: store.ui.sidebar_width,
        collapsed: store.ui.sidebar_collapsed,
        keyboard_resize: store.prefs.keyboard_resize,
        chrome: active_session_for_path(store, &BrowserWorkerPaths, path)
            .map(|session| session_chrome(store, session)),
    }
}

/// The application's frame, with the route content in the editor slot.
#[component]
pub fn AppShell(children: Element) -> Element {
    let pump = use_store();
    let location = use_location();
    let path = location();
    let compact = use_is_compact();
    let reading = read_shell(pump.core().borrow().store(), &path);
    // Re-read every render, because the dock publishes on a ResizeObserver and
    // a composer that grew does not otherwise bump the store.
    let composer = composer_geometry();
    let terminal_route = is_terminal_path(&path);
    let title = reading
        .chrome
        .as_ref()
        .and_then(|chrome| chrome.title.clone());
    let folder = reading
        .chrome
        .as_ref()
        .and_then(|chrome| chrome.folder.clone());

    // The drawer follows the route and the size class, first render included:
    // a compact home opens on the session list, anything else closes it.
    let drawer_key = (path.clone(), compact);
    let mut last_drawer_key = use_signal(|| None::<(String, bool)>);
    if last_drawer_key.peek().as_ref() != Some(&drawer_key) {
        last_drawer_key.set(Some(drawer_key));
        pump.dispatch(ClientEvent::Sidebar(drawer_intent_for_route(
            compact, &path,
        )));
    }
    #[cfg(target_arch = "wasm32")]
    {
        super::app_shell_dom::use_main_left(compact, reading.collapsed, reading.sidebar_width);
        super::app_shell_dom::use_shell_listeners(pump.clone(), compact);
        super::keyboard_offset::use_keyboard_offset();
    }
    let toggle_pump = pump.clone();
    let on_toggle_sidebar = move |()| toggle_desktop_sidebar(&toggle_pump, compact);

    rsx! {
        div {
            class: "workbench-shell",
            "data-compact": if compact { "true" } else { "false" },
            style: shell_style(reading.keyboard_resize, composer.active, reading.sidebar_width, reading.collapsed),
            if !compact {
                TitleBar { path: path.clone(), session_title: title.clone(), session_folder: folder.clone() }
                ActivityBar { on_toggle_sidebar }
                SidebarRegion { collapsed: reading.collapsed, SidebarRoot {} }
            }
            main {
                class: "workbench-editor-region",
                "data-keyboard-shift": keyboard_shift(terminal_route, reading.keyboard_resize).then_some("true"),
                style: editor_style(terminal_route, compact, reading.keyboard_resize, composer),
                if shows_mobile_top_bar(compact, &path, terminal_route) {
                    MobileTopBar { path: path.clone(), session_title: title, session_folder: folder }
                }
                div { class: "workbench-editor-slot", {children} }
            }
            if !compact {
                StatusBar {}
            }
            if compact {
                MobileSidebarDrawer { SidebarRoot {} }
            }
        }
    }
}

/// Collapse or expand the desktop rail. Focus inside the region about to be
/// hidden moves to the rail's Sessions item first, so it is never stranded in
/// an `inert` subtree.
pub fn toggle_desktop_sidebar(pump: &crate::pump::Pump, compact: bool) {
    #[cfg(target_arch = "wasm32")]
    let restore_focus = {
        let collapsed = pump.core().borrow().store().ui.sidebar_collapsed;
        super::app_shell_dom::focus_rail_before_collapse(compact, collapsed)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let _ = compact;
    pump.dispatch(ClientEvent::Sidebar(SidebarIntent::ToggleCollapsed));
    #[cfg(target_arch = "wasm32")]
    if restore_focus {
        super::app_shell_dom::refocus_rail_after_collapse();
    }
}
