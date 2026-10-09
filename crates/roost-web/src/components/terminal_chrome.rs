//! The chrome that belongs to a terminal pane rather than to the shell: the
//! permanent composer, the file picker behind its attach action and the file
//! drop that feeds the same upload, the preview strip, and the capture-consent
//! confirmation the pane's context menu raises.
//! Mounted from `CellTerminal` (or from the compact shell for the viewport
//! placement) by whoever owns the pane. The only document-wide listeners here
//! are the drag listeners of `file_drop_dom`; trusted key routing stays with
//! the app root.
//! Ports `apps/web/src/components/terminal/TerminalComposeButton.tsx` and
//! `apps/web/src/components/terminal/TerminalCaptureConsentDialog.tsx`.

pub mod attachment_picker;
pub mod capture_consent;
pub mod composer;
pub mod composer_attachments;
pub mod composer_claim;
pub mod composer_dictation;
pub mod composer_drafts;
pub mod composer_gate;
pub mod composer_geometry;
pub mod composer_key_tray;
pub mod composer_placement;
pub mod dom;
pub mod file_drop;
pub mod file_drop_dom;
pub mod pane_geometry;
pub mod pane_geometry_dom;
pub mod sent_image_strip;
pub mod short_paths;
pub mod terminal_upload;
pub mod upload;
pub mod upload_card;
pub mod upload_host;
pub mod upload_id;
pub mod upload_plan;
pub mod upload_tray;
