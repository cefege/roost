//! Terminal links: inferred-link detection over painted rows, the DOM applier
//! that wraps and validates anchors, the mutation scanner that schedules it,
//! and the attachment that owns modifier hover, the armed hold and activation.
//! The rules run natively over the `LinkDom`/`LinkScanHost`/`LinkHost` seams,
//! which `dom` (wasm32) implements for the browser.
//! Ports `apps/web/src/renderer/terminal-links.ts`.

pub mod activation;
pub mod anchor;
pub mod attachment;
pub mod detect;
#[cfg(target_arch = "wasm32")]
pub mod dom;
pub mod scan;

pub use activation::{
    LinkActivationGesture, LinkModifierKey, PressWithheld, is_link_activation_gesture,
    is_link_modifier_held, link_hint_text, link_title, withhold_press,
};
pub use anchor::{
    LinkDom, SCANNED_ATTR, apply_terminal_anchor_target, js_parse_int, linkify_terminal_rows,
    resolve_terminal_anchor_target, terminal_row_columns,
};
pub use attachment::{LinkEvent, LinkHost, LinkListener, TerminalLinks};
pub use detect::{FileLinkSegment, PaintedLink, RowLinkInput, RowLinkSegment, compute_row_links};
#[cfg(target_arch = "wasm32")]
pub use dom::TerminalLinkAttachment;
pub use scan::{DIRTY_LIMIT, FrameCallback, LinkScanHost, LinkScanner, RowSet};

/// A worker-aware file resolver, owned by the attachment.
pub type FileResolver = Box<dyn Fn(&str, Option<u64>, Option<&str>) -> Option<String>>;

/// A live read of the session's GitHub `owner/repo`, if its remote is known.
pub type OwnerRepoGetter = Box<dyn Fn() -> Option<String>>;

/// Opens a resolved worker file route in the app.
pub type FileOpener = Box<dyn Fn(&str)>;

/// v2 `TerminalLinkOpts`, plus the platform's link modifier.
pub struct TerminalLinkOptions {
    /// Resolves output paths into authenticated `/file/…` routes.
    pub resolve_file: Option<FileResolver>,
    /// Opens a resolved worker file route in the app.
    pub on_open_file: Option<FileOpener>,
    /// A getter, so scans see a Git remote that resolves after pane mount.
    pub github_owner_repo: Option<OwnerRepoGetter>,
    /// Compact keyboard-sheet arming, separate from physical modifier hover.
    pub link_activation_armed: Option<Box<dyn Fn() -> bool>>,
    /// Foreground state at construction; a hidden pane installs no link work.
    pub initial_active: bool,
    /// Holds renderer paint only while the modifier and pointer are both active.
    pub on_armed_hover_change: Option<Box<dyn Fn(bool)>>,
    /// The platform's link modifier (v2 `terminalLinkModifierKey()`).
    pub modifier_key: LinkModifierKey,
}

impl TerminalLinkOptions {
    /// v2's defaults: no resolver, no callbacks, active at construction.
    pub fn new(modifier_key: LinkModifierKey) -> Self {
        Self {
            resolve_file: None,
            on_open_file: None,
            github_owner_repo: None,
            link_activation_armed: None,
            initial_active: true,
            on_armed_hover_change: None,
            modifier_key,
        }
    }

    /// The resolver as the classifier takes it.
    pub fn file_resolver(&self) -> Option<crate::link_target::ResolveFile<'_>> {
        self.resolve_file
            .as_deref()
            .map(|resolve| resolve as crate::link_target::ResolveFile<'_>)
    }
}

impl std::fmt::Debug for TerminalLinkOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalLinkOptions")
            .field("resolve_file", &self.resolve_file.is_some())
            .field("on_open_file", &self.on_open_file.is_some())
            .field("github_owner_repo", &self.github_owner_repo.is_some())
            .field(
                "link_activation_armed",
                &self.link_activation_armed.is_some(),
            )
            .field("initial_active", &self.initial_active)
            .field(
                "on_armed_hover_change",
                &self.on_armed_hover_change.is_some(),
            )
            .field("modifier_key", &self.modifier_key)
            .finish()
    }
}
