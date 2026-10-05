//! What a drag over the page means: whether the browser's own drop (opening
//! the file, following the link) is suppressed, and which terminal pane takes
//! the drop and how. Target-independent so the rules are tested natively; the
//! listeners that apply them are `file_drop_dom`. Ports the drop half of
//! `apps/web/src/components/terminal/cell-terminal-interactions.ts`.

/// The `DataTransfer.types` entry a drag of one or more files carries.
const FILES_TYPE: &str = "Files";
/// The `DataTransfer.types` entry a dragged link or image URL carries.
const URI_LIST_TYPE: &str = "text/uri-list";

/// The payloads a drag announces before its data is readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DragKinds {
    /// The drag carries files: from the desktop, or an image another page
    /// handed over as a file.
    pub files: bool,
    /// The drag carries a URL.
    pub link: bool,
}

impl DragKinds {
    /// Classify a drag by its `DataTransfer.types`.
    pub fn from_types<Types, Name>(types: Types) -> Self
    where
        Types: IntoIterator<Item = Name>,
        Name: AsRef<str>,
    {
        let mut kinds = Self::default();
        for name in types {
            match name.as_ref() {
                FILES_TYPE => kinds.files = true,
                URI_LIST_TYPE => kinds.link = true,
                _ => {}
            }
        }
        kinds
    }
}

/// Whether the page cancels the browser's default for this drag.
///
/// A file dropped anywhere is suppressed — the browser's default is to leave
/// the app for the file, even over a text field. A link is suppressed except
/// over an editable element, whose default of inserting the URL as text is
/// the useful one. A plain text drag is left alone everywhere.
pub fn page_blocks_default(kinds: DragKinds, target_editable: bool) -> bool {
    kinds.files || (kinds.link && !target_editable)
}

/// Whether a pane takes a drag that is over `over_pane`, the session id of the
/// terminal pane under the pointer.
///
/// A drag over a pane belongs to that pane. A drag over anything else — the
/// portaled composer, the chrome around the deck — belongs to the focused
/// pane, as every drop did in v2.
pub fn pane_claims(over_pane: Option<&str>, own_session_id: &str, focused: bool) -> bool {
    match over_pane {
        Some(session_id) => session_id == own_session_id,
        None => focused,
    }
}

/// What a claiming pane does with a drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDropAction {
    /// Upload the files exactly as the attach button would.
    Upload,
    /// Paste the dropped URL into the terminal, through the paste guard.
    PasteLink,
    /// Leave the drop to the element under it.
    Decline,
}

/// Decide a claimed pane's action.
///
/// A drag that started inside this page is the user rearranging the page, not
/// handing the terminal something, so it is declined whatever it carries. A
/// link over an editable element is declined so the field inserts it.
pub fn pane_drop_action(kinds: DragKinds, target_editable: bool, internal: bool) -> PaneDropAction {
    if internal {
        return PaneDropAction::Decline;
    }
    if kinds.files {
        return PaneDropAction::Upload;
    }
    if kinds.link && !target_editable {
        return PaneDropAction::PasteLink;
    }
    PaneDropAction::Decline
}

/// The text a dropped `text/uri-list` pastes: its URLs separated by spaces,
/// with the list's `#` comment lines dropped. `None` when it names no URL.
pub fn uri_list_text(raw: &str) -> Option<String> {
    let urls: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    (!urls.is_empty()).then(|| urls.join(" "))
}
