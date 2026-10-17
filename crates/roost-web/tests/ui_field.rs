//! The field help-id rule: a control's `aria-describedby` lists the caller's ids
//! first, then the help text, then the error, skipping any that are absent.

use roost_web::components::ui::field::described_by;

#[test]
fn a_description_reads_the_caller_then_the_help_then_the_error() {
    assert_eq!(described_by(None, None, None), None);
    assert_eq!(described_by(Some(""), None, None), None);
    assert_eq!(
        described_by(Some("hint"), Some("f-description"), Some("f-error")).as_deref(),
        Some("hint f-description f-error")
    );
    assert_eq!(
        described_by(None, None, Some("f-error")).as_deref(),
        Some("f-error")
    );
}
