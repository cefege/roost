//! The workbench grid: title region, activity rail, sidebar region, editor slot,
//! status bar, and the compact bar that replaces the rail on a phone. Ported
//! from `apps/web/src/components/layout/AppShell.tsx`.
//!
//! The grid itself, the `data-compact` / `data-collapsed` / `data-keyboard-shift`
//! attributes and the three sidebar-width custom properties are the whole
//! contract with `assets/styles/workbench-shell.css`; the components inside it
//! are this module's siblings.
//!
//! The sidebar region is a SLOT, not a sidebar. The region owns the width, the
//! collapse and the resizer; the sidebar surface mounts as its children. A
//! sidebar that grew its own width logic would be a second answer to a question
//! the grid already answers.
//!
//! NO INLINE GEOMETRY. The only style this module writes is the three sidebar
//! width custom properties, and each is emitted from `shell_metrics` or from the
//! store's own width. Height, the soft-keyboard shift and the composer's resting
//! row are the stylesheet's, because a component that also decided them would be
//! a second source for a PTY resize — and one PTY resize repaints a full-screen
//! TUI in place.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientCore;

use super::activity_bar::ActivityBar;
use super::mobile_bar::MobileTopBar;
use super::shell_metrics::{SizeClass, classify};
use super::sidebar_region::SidebarRegion;
use super::status_bar::StatusBar;
use super::title_bar::TitleBar;

/// The CSS custom property the grid reads the collapsed rail's width from.
///
/// A collapsed sidebar is zero WIDTH, not zero content: the region keeps its
/// grid area so the editor does not reflow on every collapse, and the stylesheet
/// reads this to collapse what is inside it.
pub const SIDEBAR_WIDTH_VAR: &str = "--workbench-sidebar-width";

/// The custom property holding the sidebar's width when it is expanded.
pub const SIDEBAR_EXPANDED_WIDTH_VAR: &str = "--workbench-sidebar-expanded-width";

/// The custom property the resizer's active width is read from, so a collapsed
/// sidebar cannot be dragged.
pub const SIDEBAR_RESIZER_ACTIVE_WIDTH_VAR: &str = "--workbench-sidebar-resizer-active-width";

/// The three sidebar-width custom properties, as a style string.
///
/// All three are emitted together because they are one fact: a collapsed sidebar
/// is a width of zero in the grid, a zero in the resizer, and its remembered
/// width still held in the expanded property so expanding restores it.
pub fn sidebar_width_style(expanded_px: u32, collapsed: bool) -> String {
    let expanded = format!("{expanded_px}px");
    if collapsed {
        format!(
            "{SIDEBAR_EXPANDED_WIDTH_VAR}: {expanded}; \
             {SIDEBAR_WIDTH_VAR}: 0px; \
             {SIDEBAR_RESIZER_ACTIVE_WIDTH_VAR}: 0px;"
        )
    } else {
        format!(
            "{SIDEBAR_EXPANDED_WIDTH_VAR}: {expanded}; \
             {SIDEBAR_WIDTH_VAR}: var({SIDEBAR_EXPANDED_WIDTH_VAR}); \
             {SIDEBAR_RESIZER_ACTIVE_WIDTH_VAR}: var(--workbench-sidebar-resizer-width);"
        )
    }
}

/// The application's frame, with the route content in the editor slot.
#[component]
pub fn AppShell(path: String, on_navigate: EventHandler<String>, children: Element) -> Element {
    let core = use_context::<Rc<RefCell<ClientCore>>>();
    let (width, height) = use_hook(viewport);
    let size_class = use_hook(move || classify(width, height));
    let (sidebar_width, collapsed) = use_hook(|| read_sidebar(&core));
    let session = use_hook(|| read_session(&path, &core));
    let terminal_route = is_terminal_route(&path);
    let compact = size_class == SizeClass::Compact;
    let title = session.as_ref().and_then(|session| session.title.clone());
    let folder = session.map(|session| session.folder);

    rsx! {
        div {
            class: "workbench-shell",
            "data-compact": if compact { "true" } else { "false" },
            style: sidebar_width_style(sidebar_width, collapsed),
            TitleBar { path: path.clone(), on_navigate, session_title: title.clone(), session_folder: folder.clone() }
            if !compact {
                ActivityBar { path: path.clone(), on_navigate }
                SidebarRegion { collapsed, children: rsx! {} }
            }
            main {
                class: "workbench-editor-region",
                "data-keyboard-shift": if terminal_route { "true" } else { "false" },
                if compact {
                    MobileTopBar { path: path.clone(), session_title: title, session_folder: folder }
                }
                div { class: "workbench-editor-slot", {children} }
            }
            if !compact {
                StatusBar { path: path.clone() }
            }
        }
    }
}

/// Whether this path is a terminal route.
///
/// Drives `data-keyboard-shift`, which translates the editor region for a soft
/// keyboard. It is a PREFIX test rather than a `Route` match because the shift
/// applies to the three terminal route shapes and to nothing else — and because
/// it must also apply to a terminal route that resolved to no session, since a
/// pane that is still loading needs the keyboard just as much.
pub fn is_terminal_route(path: &str) -> bool {
    path.starts_with("/s/") || path.starts_with("/t/") || path.starts_with("/w/")
}

/// The sidebar's remembered width and whether it is collapsed.
fn read_sidebar(core: &Rc<RefCell<ClientCore>>) -> (u32, bool) {
    let store = core.borrow();
    let store = store.store();
    (store.ui.sidebar_width, store.ui.sidebar_collapsed)
}

/// What the chrome shows about the session a path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChrome {
    /// The session's own title, when it has one.
    pub title: Option<String>,
    /// The folder it runs in, which the title bar falls back to.
    pub folder: String,
}

/// Read the session a terminal path names, when it names one exactly.
///
/// `/t/:workerFp/*folderPath` and `/w/:workspaceId` name a folder and a
/// workspace, not a session id. Resolving those needs the folder index and its
/// newest-wins tiebreak, which belongs to the terminal surface — so the chrome
/// asks only the question it can answer exactly and reads the route's own name
/// for the rest.
fn read_session(path: &str, core: &Rc<RefCell<ClientCore>>) -> Option<SessionChrome> {
    if !is_terminal_route(path) {
        return None;
    }
    let wanted = path.strip_prefix("/s/")?;
    let wanted = wanted.split(['/', '?', '#']).next().unwrap_or(wanted);
    if wanted.is_empty() {
        return None;
    }
    let borrowed = core.borrow();
    let store = borrowed.store();
    let session = store
        .sessions
        .sessions()
        .values()
        .find(|session| session.id.as_str() == wanted)?;
    Some(SessionChrome {
        title: session.custom_title.clone(),
        folder: session
            .spawn_cwd
            .clone()
            .unwrap_or_else(|| session.cwd.clone()),
    })
}

/// The viewport, in CSS pixels.
#[cfg(target_arch = "wasm32")]
fn viewport() -> (u32, u32) {
    web_sys::window().map_or((0, 0), |window| {
        (
            window.inner_width().max(0) as u32,
            window.inner_height().max(0) as u32,
        )
    })
}

/// A native build has no viewport; the root classifies as desktop, which is the
/// side of the boundary whose chrome is the larger of the two.
#[cfg(not(target_arch = "wasm32"))]
fn viewport() -> (u32, u32) {
    (1280, 900)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collapsed_sidebar_is_zero_in_the_grid_and_in_the_resizer() {
        let style = sidebar_width_style(300, true);
        assert!(style.contains("--workbench-sidebar-expanded-width: 300px"));
        assert!(style.contains("--workbench-sidebar-width: 0px"));
        assert!(style.contains("--workbench-sidebar-resizer-active-width: 0px"));
    }

    #[test]
    fn an_expanded_sidebar_keeps_its_width_in_the_expanded_property() {
        // The expanded width is held even while collapsed, which is what makes
        // expanding restore the width the reader chose rather than the default.
        let style = sidebar_width_style(280, false);
        assert!(style.contains("--workbench-sidebar-expanded-width: 280px"));
        assert!(style.contains("--workbench-sidebar-width: var("));
    }

    #[test]
    fn an_expanded_resizer_reads_its_own_token_and_not_a_number() {
        // A number here would pin the resizer's grab area to the width at first
        // paint and leave the stylesheet's own token meaning nothing.
        let style = sidebar_width_style(280, false);
        assert!(style.contains("--workbench-sidebar-resizer-active-width: var("));
    }

    #[test]
    fn only_the_three_width_properties_are_emitted() {
        // The grid's height is the stylesheet's business. A component that also
        // wrote a height would be a second answer to the soft-keyboard question,
        // and a PTY resize repaints a full-screen TUI in place.
        let style = sidebar_width_style(280, false);
        // Count the NAMES, not the `--` tokens: a property whose value is
        // `var(--other)` contributes two tokens, so counting tokens tests an
        // arithmetic detail of the spelling rather than the property itself.
        let names: Vec<&str> = style
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("--"))
            .collect();
        assert_eq!(names.len(), 3, "three custom properties, got {names:?}");
        for expected in [
            SIDEBAR_EXPANDED_WIDTH_VAR,
            SIDEBAR_WIDTH_VAR,
            SIDEBAR_RESIZER_ACTIVE_WIDTH_VAR,
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        // A `var()` REFERENCE is not a declaration, so the list above stays three
        // while the string still names five tokens.
        assert!(style.contains("var(--workbench-sidebar-expanded-width)"));
        assert!(!style.contains("height"));
    }

    #[test]
    fn a_session_route_shifts_the_editor_for_a_soft_keyboard() {
        for path in ["/s/abc", "/t/fp1/src", "/w/ws1", "/w/ws1/t/ch1"] {
            assert!(is_terminal_route(path), "{path}");
        }
    }

    #[test]
    fn a_non_terminal_route_never_shifts_the_editor() {
        // Shifting a settings pane or a file view would move it under a keyboard
        // that has nothing to type into.
        for path in ["/", "/search", "/settings/machines", "/browse", "/help"] {
            assert!(!is_terminal_route(path), "{path}");
        }
    }

    #[test]
    fn a_root_path_is_not_a_terminal_route_even_though_the_rail_calls_it_one() {
        // The rail's Sessions destination owns the root, but the soft-keyboard
        // shift does not: the landing page has no PTY to protect.
        assert!(!is_terminal_route("/"));
    }

    #[test]
    fn a_route_naming_no_session_yields_no_chrome_rather_than_a_guess() {
        // `/t/:workerFp/*folderPath` names a folder, not a session. Guessing here
        // would put an unrelated session's title in the title bar.
        let core = Rc::new(RefCell::new(ClientCore::in_memory("tab-test")));
        assert_eq!(read_session("/t/fp12345/src", &core), None);
        assert_eq!(read_session("/w/ws1", &core), None);
        assert_eq!(read_session("/settings", &core), None);
        assert_eq!(read_session("/s/", &core), None);
    }
}
