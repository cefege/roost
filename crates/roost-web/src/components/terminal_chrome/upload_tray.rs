//! Pure upload-tray labels and lifecycle state.
//! The composer uses these helpers to describe staged files without coupling
//! their display rules to browser APIs or the transfer store.

/// The user-visible upload lifecycle before and after Send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadRowState {
    /// File is waiting in the composer's attachment tray.
    Staged,
    /// User sent the staged set; the serial upload queue has not started it.
    Queued,
    /// Bytes are being sent.
    Sending,
    /// Upload completed successfully.
    Sent,
    /// Existing worker content was reused.
    Reused,
    /// Upload was definitively rejected.
    Failed,
    /// Upload result is unknown and must not be retried automatically.
    UnknownResult,
}

impl UploadRowState {
    /// Return the next lifecycle state when the edge is permitted.
    pub fn transition(self, next: Self) -> Option<Self> {
        let allowed = matches!(
            (self, next),
            (Self::Staged, Self::Queued)
                | (Self::Staged, Self::Failed)
                | (Self::Queued, Self::Sending)
                | (Self::Queued, Self::Failed)
                | (Self::Sending, Self::Sent)
                | (Self::Sending, Self::Reused)
                | (Self::Sending, Self::Failed)
                | (Self::Sending, Self::UnknownResult)
        );
        allowed.then_some(next)
    }
}

/// Derive a compact uppercase thumbnail label from a file name.
pub fn extension_label(name: &str) -> String {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "FILE".to_owned();
    };
    if extension.is_empty() {
        return "FILE".to_owned();
    }
    extension.to_uppercase().chars().take(4).collect()
}

/// Format a byte count for a staged attachment row.
pub fn format_file_size(bytes: u64) -> String {
    crate::display_format::format_bytes(bytes as f64)
}

#[cfg(test)]
mod tests {
    use super::{UploadRowState, extension_label, format_file_size};

    #[test]
    fn extension_labels_cover_dotfiles_and_compound_names() {
        assert_eq!(extension_label(".env"), "ENV");
        assert_eq!(extension_label("README"), "FILE");
        assert_eq!(extension_label("archive.tar.gz"), "GZ");
        assert_eq!(extension_label("long.extension"), "EXTE");
        assert_eq!(extension_label("notes."), "FILE");
    }

    #[test]
    fn upload_rows_only_follow_valid_lifecycle_edges() {
        assert_eq!(
            UploadRowState::Staged.transition(UploadRowState::Queued),
            Some(UploadRowState::Queued)
        );
        assert_eq!(
            UploadRowState::Queued.transition(UploadRowState::Sending),
            Some(UploadRowState::Sending)
        );
        assert_eq!(
            UploadRowState::Sending.transition(UploadRowState::UnknownResult),
            Some(UploadRowState::UnknownResult)
        );
        assert_eq!(
            UploadRowState::UnknownResult.transition(UploadRowState::Queued),
            None
        );
        assert_eq!(
            UploadRowState::Sending.transition(UploadRowState::Sent),
            Some(UploadRowState::Sent)
        );
        assert_eq!(
            UploadRowState::Sending.transition(UploadRowState::Reused),
            Some(UploadRowState::Reused)
        );
        assert_eq!(
            UploadRowState::Sending.transition(UploadRowState::Failed),
            Some(UploadRowState::Failed)
        );
        assert_eq!(
            UploadRowState::Sent.transition(UploadRowState::Queued),
            None
        );
        assert_eq!(
            UploadRowState::Sent.transition(UploadRowState::Sending),
            None
        );
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1_024), "1.0 KB");
        assert_eq!(format_file_size(1_500_000), "1.4 MB");
    }
}
