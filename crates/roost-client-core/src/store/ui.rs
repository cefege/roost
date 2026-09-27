//! UI chrome: the sidebar, the mobile drawer, and the folder view.
//!
//! Kept out of the wire-shaped state on purpose (`uiStore.ts:1-4`): transient
//! chrome must not churn the records the Sync projector folds, and a sidebar
//! that re-renders because a drawer opened is a repaint a user can see.
//!
//! Four of the five fields persist per device, and the persistence goes through
//! the same functions that change the value, so a stored value and its
//! in-memory value cannot drift. `sidebar_open` does not persist: it is the
//! mobile drawer, and a reload on a phone should not open a drawer over the page
//! the user asked for.
//!
//! Every range here is bounded at BOTH ends from the same constant pair, on the
//! way in and on the way out of storage. A stored width is a string some earlier
//! build wrote and some later build reads, and a bound applied on only one side
//! is how a sidebar ends up 4 px or 4000 px wide.
//!
//! Ported from `apps/web/src/store/uiStore.ts`.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::{read_flag, write_flag};

/// The stored keys. Named, because a key that is written in one function and
/// read by a literal in another is a preference that silently stops persisting.
pub const SIDEBAR_COLLAPSED_KEY: &str = "roost.sidebarCollapsed";
/// See [`SIDEBAR_COLLAPSED_KEY`].
pub const SIDEBAR_WIDTH_KEY: &str = "roost.sidebarWidth";
/// See [`SIDEBAR_COLLAPSED_KEY`].
pub const SIDEBAR_VIEW_KEY: &str = "roost.sidebarView";
/// See [`SIDEBAR_COLLAPSED_KEY`].
pub const HOME_FOLDER_SHOW_FILES_KEY: &str = "roost.homeFolderShowFiles";

/// The sidebar width a device gets before it has chosen one.
pub const SIDEBAR_WIDTH_DEFAULT: u32 = 300;
/// The narrowest a drag may take the sidebar. Below this the session rows clip.
pub const SIDEBAR_WIDTH_MIN: u32 = 200;
/// The widest a drag may take it. Beyond this the terminal loses the pane.
pub const SIDEBAR_WIDTH_MAX: u32 = 600;

/// Which retained projection the sidebar is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarView {
    /// Folders, with the sessions inside them.
    #[default]
    Folders,
    /// Agents, with their status.
    Agents,
}

impl SidebarView {
    /// The stored spelling. The only two values ever written.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Folders => "folders",
            Self::Agents => "agents",
        }
    }

    /// Read a stored value. Anything else is `folders`: a value no build wrote
    /// is not a preference, and refusing it closed is the only safe reading.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("agents") => Self::Agents,
            _ => Self::Folders,
        }
    }
}

/// The chrome state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiState {
    /// The mobile drawer. Ignored on a desktop, where the sidebar is always
    /// visible.
    pub sidebar_open: bool,
    /// Whether the desktop sidebar is collapsed to its rail.
    pub sidebar_collapsed: bool,
    /// Which retained projection the sidebar shows.
    pub sidebar_view: SidebarView,
    /// The sidebar's width in pixels, drag-resizable.
    pub sidebar_width: u32,
    /// Whether the home and browse surfaces reveal view-only files beside
    /// folders.
    pub home_folder_show_files: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            sidebar_open: false,
            sidebar_collapsed: false,
            sidebar_view: SidebarView::Folders,
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            home_folder_show_files: false,
        }
    }
}

impl UiState {
    /// The defaults, before storage is read.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Clamp a width to the range a drag may take, rounding to whole pixels.
///
/// One function, applied by the setter AND by the loader, from the same
/// constants. A width below the minimum clips the session rows; a width above
/// the maximum takes the pane away, and neither is a preference the user chose.
pub fn clamp_sidebar_width(px: u32) -> u32 {
    px.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
}

/// Read every persisted chrome value, falling back to the default for anything
/// storage cannot answer.
///
/// Total by construction: a storage backend that throws is a host problem, and
/// the caller already handles a `None` from `get`. A stored value that is not
/// the value this build writes is ignored rather than trusted.
pub fn load_ui(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    let stored_width = storage
        .get(SIDEBAR_WIDTH_KEY)
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .map_or(SIDEBAR_WIDTH_DEFAULT, clamp_sidebar_width);
    let next = UiState {
        sidebar_open: store.ui.sidebar_open,
        sidebar_collapsed: read_flag(storage, SIDEBAR_COLLAPSED_KEY, false),
        sidebar_view: SidebarView::parse(storage.get(SIDEBAR_VIEW_KEY).as_deref()),
        sidebar_width: stored_width,
        home_folder_show_files: read_flag(storage, HOME_FOLDER_SHOW_FILES_KEY, false),
    };
    if next == store.ui {
        return false;
    }
    store.ui = next;
    store.note_change();
    true
}

/// Open the mobile drawer.
pub fn open_sidebar(store: &mut Store) -> bool {
    set_sidebar_open(store, true)
}

/// Close the mobile drawer.
pub fn close_sidebar(store: &mut Store) -> bool {
    set_sidebar_open(store, false)
}

fn set_sidebar_open(store: &mut Store, open: bool) -> bool {
    if store.ui.sidebar_open == open {
        return false;
    }
    store.ui.sidebar_open = open;
    store.note_change();
    true
}

/// Flip the desktop sidebar between its rail and its full width, and persist.
pub fn toggle_sidebar_collapsed(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    let next = !store.ui.sidebar_collapsed;
    store.ui.sidebar_collapsed = next;
    write_flag(storage, SIDEBAR_COLLAPSED_KEY, next);
    store.note_change();
    tracing::debug!(target: "store", collapsed = next, "sidebar collapsed");
    true
}

/// Choose the sidebar's retained projection, and persist.
///
/// No-ops on the value already shown, so a re-render that re-asserts the current
/// view does not write storage or repaint.
pub fn set_sidebar_view(store: &mut Store, storage: &dyn KeyValueStore, view: SidebarView) -> bool {
    if store.ui.sidebar_view == view {
        return false;
    }
    let from = store.ui.sidebar_view;
    store.ui.sidebar_view = view;
    storage.set(SIDEBAR_VIEW_KEY, view.as_str());
    store.note_change();
    tracing::debug!(target: "store", from = from.as_str(), to = view.as_str(), "sidebar view");
    true
}

/// Drag the sidebar to a new width, and persist.
pub fn set_sidebar_width(store: &mut Store, storage: &dyn KeyValueStore, px: u32) -> bool {
    let clamped = clamp_sidebar_width(px);
    if store.ui.sidebar_width == clamped {
        return false;
    }
    store.ui.sidebar_width = clamped;
    storage.set(SIDEBAR_WIDTH_KEY, &clamped.to_string());
    store.note_change();
    true
}

/// Whether the home and browse surfaces reveal view-only files, and persist.
pub fn set_home_folder_show_files(
    store: &mut Store,
    storage: &dyn KeyValueStore,
    show: bool,
) -> bool {
    if store.ui.home_folder_show_files == show {
        return false;
    }
    store.ui.home_folder_show_files = show;
    write_flag(storage, HOME_FOLDER_SHOW_FILES_KEY, show);
    store.note_change();
    true
}
