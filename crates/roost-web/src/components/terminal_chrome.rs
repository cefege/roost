//! The chrome that belongs to a terminal pane rather than to the shell: the
//! permanent composer, the file picker behind its attach action, the preview
//! strip, and the capture-consent confirmation the pane's context menu raises.
//! Mounted from `CellTerminal` (or from the compact shell for the viewport
//! placement) by whoever owns the pane; nothing here installs a document-wide
//! listener, because the app root already owns trusted key routing.
//! Ports `apps/web/src/components/terminal/TerminalComposeButton.tsx` and
//! `apps/web/src/components/terminal/TerminalCaptureConsentDialog.tsx`.

pub mod attachment_picker;
pub mod capture_consent;
pub mod composer;
pub mod composer_claim;
pub mod composer_dictation;
pub mod composer_drafts;
pub mod composer_gate;
pub mod composer_geometry;
pub mod dom;
pub mod pane_geometry;
pub mod pane_geometry_dom;
pub mod short_paths;
pub mod upload;
pub mod upload_card;
pub mod upload_host;
pub mod upload_id;
pub mod upload_plan;
