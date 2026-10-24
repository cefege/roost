//! What a ceremony stage SAYS: the status dot, the sentence, and the labels the
//! request card's buttons carry.
//!
//! Owned by the pairing surface and read by both halves of it — the requester's
//! card and the approver's code dialog — because the two describe the same
//! server statuses, and a card that spelled `verification_failed` one way and
//! the dialog another would be a second answer to one question. The seven
//! sentences are v2's verbatim (`OnboardingRequestCard.tsx:83-97`); the specs
//! read them.
//!
//! Every function here is pure so the wording is testable natively, where there
//! is no document to select out of.

use roost_client_core::client::auth::PairPollStatus;

/// The `StatusDot` status name for a stage.
///
/// A failure sentence outranks the stage: a confirmation refused inside
/// `verification_required` is an error even though the stage is not, and v2
/// reads the same way (`OnboardingRequestCard.tsx:33-46`).
pub fn poll_indicator(
    status: &PairPollStatus,
    failure: Option<&str>,
    failed: bool,
) -> &'static str {
    if failure.is_some() || failed {
        return "error";
    }
    match status {
        PairPollStatus::Completed => "ok",
        PairPollStatus::Denied | PairPollStatus::Expired | PairPollStatus::VerificationFailed => {
            "warn"
        }
        PairPollStatus::Unknown(_) => "error",
        PairPollStatus::Idle | PairPollStatus::Pending | PairPollStatus::VerificationRequired => {
            "info"
        }
    }
}

/// The sentence the status line shows, or `None` when it should show none.
///
/// A denial, an expiry, a verification failure and a completion each get their
/// own sentence: they are four different facts about the request, and a single
/// "pairing failed" across three of them would send a reader off to re-pair for
/// a reason that was somebody else's decision.
pub fn poll_message(
    status: &PairPollStatus,
    failure: Option<&str>,
    failed: bool,
) -> Option<String> {
    if let Some(failure) = failure {
        return Some(failure.to_string());
    }
    let message = match status {
        PairPollStatus::Idle => return None,
        PairPollStatus::Pending => "Waiting for approval on another paired browser…",
        PairPollStatus::VerificationRequired => {
            "Approval received. Enter the 6-digit code shown on the paired browser."
        }
        PairPollStatus::Completed => "Pairing completed.",
        PairPollStatus::Denied => "Request denied.",
        PairPollStatus::Expired => "Request expired — request again.",
        PairPollStatus::VerificationFailed => "Too many incorrect codes — request again.",
        PairPollStatus::Unknown(name) => {
            return Some(format!(
                "The coordinator reported an unknown pairing status ({name})."
            ));
        }
    };
    if failed {
        // v2's `pollStatus === "error"` copy, reached only while the page's own
        // notice is not already saying the same thing more precisely.
        return Some("Request failed — request again.".to_string());
    }
    Some(message.to_string())
}

/// The primary button's label before anything has been requested, or after a
/// failure that asking again can fix.
pub fn request_start_label(busy: bool, failed: bool) -> &'static str {
    if busy {
        "Requesting…"
    } else if failed {
        "Request again"
    } else {
        "Request approval"
    }
}

/// Whether the card offers anything after the status line.
///
/// `idle` has no request to act on, and `pending` has nothing a reader can do
/// but wait — offering a control that only restarts what is already running is
/// a control that lies about what it will do.
pub const fn shows_request_actions(status: &PairPollStatus) -> bool {
    !matches!(status, PairPollStatus::Idle | PairPollStatus::Pending)
}

/// The secondary button's label, which offers a fresh request rather than
/// continuing the one on screen.
pub const fn request_restart_label(status: &PairPollStatus) -> &'static str {
    if matches!(status, PairPollStatus::VerificationRequired) {
        "Start over"
    } else {
        "Request again"
    }
}

/// The six digits grouped as `123 456` for the approver's dialog.
///
/// Grouped because the code is read aloud and typed back by a person; six
/// unbroken digits is a transcription error waiting to happen. Anything that is
/// not exactly six digits is rendered as it came in, so a malformed value stays
/// visible rather than being silently mangled.
pub fn group_verification_code(code: &str) -> String {
    let digits = code.bytes().all(|byte| byte.is_ascii_digit());
    match (digits, code.split_at_checked(3)) {
        (true, Some((head, tail))) if tail.len() == 3 => format!("{head} {tail}"),
        _ => code.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_outranks_the_stage_it_happened_in() {
        let status = PairPollStatus::VerificationRequired;
        assert_eq!(poll_indicator(&status, None, false), "info");
        assert_eq!(
            poll_indicator(&status, Some("That code did not match."), false),
            "error"
        );
        assert_eq!(
            poll_message(&status, Some("That code did not match."), false),
            Some("That code did not match.".to_string())
        );
    }

    #[test]
    fn the_seven_sentences_the_specs_read_are_all_reachable() {
        let said = [
            poll_message(&PairPollStatus::Pending, None, false),
            poll_message(&PairPollStatus::VerificationRequired, None, false),
            poll_message(&PairPollStatus::Completed, None, false),
            poll_message(&PairPollStatus::Denied, None, false),
            poll_message(&PairPollStatus::Expired, None, false),
            poll_message(&PairPollStatus::VerificationFailed, None, false),
            poll_message(&PairPollStatus::Pending, None, true),
        ];
        assert_eq!(
            said[0].as_deref(),
            Some("Waiting for approval on another paired browser…")
        );
        assert_eq!(
            said[1].as_deref(),
            Some("Approval received. Enter the 6-digit code shown on the paired browser.")
        );
        assert_eq!(said[2].as_deref(), Some("Pairing completed."));
        assert_eq!(said[3].as_deref(), Some("Request denied."));
        assert_eq!(said[4].as_deref(), Some("Request expired — request again."));
        assert_eq!(
            said[5].as_deref(),
            Some("Too many incorrect codes — request again.")
        );
        assert_eq!(said[6].as_deref(), Some("Request failed — request again."));
    }

    #[test]
    fn a_status_this_client_does_not_know_is_named_rather_than_ignored() {
        let status = PairPollStatus::Unknown("awaiting_signature".to_string());
        assert_eq!(poll_indicator(&status, None, false), "error");
        assert_eq!(
            poll_message(&status, None, false),
            Some(
                "The coordinator reported an unknown pairing status (awaiting_signature)."
                    .to_string()
            )
        );
    }

    #[test]
    fn an_id_request_has_no_status_line_and_no_actions() {
        assert_eq!(poll_message(&PairPollStatus::Idle, None, false), None);
        assert!(!shows_request_actions(&PairPollStatus::Idle));
        assert!(!shows_request_actions(&PairPollStatus::Pending));
        assert!(shows_request_actions(&PairPollStatus::VerificationRequired));
        assert!(shows_request_actions(&PairPollStatus::Denied));
    }

    #[test]
    fn only_six_digits_are_grouped() {
        assert_eq!(group_verification_code("123456"), "123 456");
        assert_eq!(group_verification_code("12345"), "12345");
        assert_eq!(group_verification_code("12345x"), "12345x");
        assert_eq!(group_verification_code(""), "");
    }
}
