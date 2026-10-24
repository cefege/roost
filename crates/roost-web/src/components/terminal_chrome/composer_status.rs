//! Submission feedback state owned by the terminal composer.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmissionStatus {
    /// The sentence the status line shows, or `None` when it shows nothing.
    pub message: Option<String>,
    /// Whether a refusal put the draft back, so the user can fix it and resend.
    pub restore_draft: bool,
}
