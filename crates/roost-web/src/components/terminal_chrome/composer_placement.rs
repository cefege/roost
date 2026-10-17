//! Where a terminal composer is mounted: portaled above the compact shell's
//! bottom chrome, or inside the pane on a shell with its own status bar.
//! Read by `composer` (positioning), `composer_gate` (visibility under the
//! drawer) and `composer_claim` (the shell's reserved height).

/// Where the composer is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerPlacement {
    /// Portaled to the document, above the status bar, for the compact shell.
    #[default]
    Viewport,
    /// Inside the pane, for a viewport wide enough to have its own status bar.
    Pane,
}

impl ComposerPlacement {
    /// The `data-placement` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewport => "viewport",
            Self::Pane => "pane",
        }
    }
}
