//! What the picker's page hands its four bands and two dialogs: the handlers
//! themselves, and the focus rules the sheet's own auto-focus would otherwise
//! swallow. Split from `picker::page` for the line cap.
//!
//! Called by `browse::picker::page` and `picker::regions`.

use dioxus::prelude::*;

use crate::components::browse::dom;
use crate::components::md::focus_scope::AutoFocusRequest;

/// Every handler the four bands and the two dialogs report back through.
#[derive(Clone, PartialEq)]
pub struct PickerControls {
    /// The crumb trail the path band paints.
    pub crumb_views: Vec<crate::platform::worker_paths::palette::CrumbView>,
    /// The title the toolbar shows.
    pub folder_name: String,
    /// The recents strip the entry region scrolls above the grid.
    pub header: Option<Element>,
    /// Whether the machine is absent from a hydrated registry.
    pub unavailable: bool,
    /// Leave the picker.
    pub on_close: EventHandler<()>,
    /// Show or hide the filter box.
    pub on_toggle_filter: EventHandler<()>,
    /// Show or hide files beside folders.
    pub on_toggle_show_files: EventHandler<()>,
    /// Open the new-folder dialog.
    pub on_new_folder: EventHandler<()>,
    /// Browse another machine.
    pub on_select_server: EventHandler<String>,
    /// Show or hide the machine switcher.
    pub on_toggle_server_menu: EventHandler<()>,
    /// Hide the machine switcher.
    pub on_close_server_menu: EventHandler<()>,
    /// Navigate to a breadcrumb's path.
    pub on_navigate: EventHandler<String>,
    /// Go back in this machine's history.
    pub on_back: EventHandler<()>,
    /// Go forward in this machine's history.
    pub on_forward: EventHandler<()>,
    /// Go to the parent directory.
    pub on_up: EventHandler<()>,
    /// Go to this machine's home.
    pub on_home: EventHandler<()>,
    /// Type into the filter.
    pub on_filter: EventHandler<String>,
    /// Hide the filter and clear it.
    pub on_close_filter: EventHandler<()>,
    /// Show or hide the crumb overflow menu.
    pub on_toggle_crumb_menu: EventHandler<()>,
    /// Hide the crumb overflow menu.
    pub on_close_crumb_menu: EventHandler<()>,
    /// Descend into a listed folder.
    pub on_drill: EventHandler<String>,
    /// Ask the machine for the directory again.
    pub on_retry: EventHandler<()>,
    /// Open a terminal in the directory being browsed.
    pub on_open_here: EventHandler<()>,
    /// Go home from the unavailable panel.
    pub on_go_home: EventHandler<()>,
    /// Type a new folder's name.
    pub on_new_folder_name: EventHandler<String>,
    /// Close the new-folder dialog.
    pub on_close_new_folder: EventHandler<()>,
    /// Create the folder the dialog names.
    pub on_create_folder: EventHandler<()>,
}

/// Put focus where the page wants it, which is never the sheet's own first
/// tabbable: the entry region's arrows and the unavailable panel's Tab are both
/// reader affordances the dialog's default would swallow.
pub fn focus_region(unavailable: bool) {
    dom::focus_by_id(if unavailable {
        crate::components::browse::unavailable::REGION_ID
    } else {
        dom::RESULTS_ID
    });
}

/// Keep the sheet from taking focus on open.
pub fn keep_sheet_focus(request: AutoFocusRequest) {
    request.prevent_default();
}
