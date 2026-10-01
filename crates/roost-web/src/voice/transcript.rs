//! How dictation words are painted into a draft the user is also typing in.
//!
//! The field is never handed the provisional words: the settled text is
//! appended to the real draft, and the hypothesis rides in a ghost mirror laid
//! exactly over the field, so a wrong guess can be dropped without ever having
//! touched the value. Ports `apps/web/src/components/terminal/TerminalComposeDictation.ts`
//! (`glued`, `paintDictation`, `provisionalFrom`).

use super::state::LiveTranscript;

/// Append speech to a draft without gluing a word onto the last one.
///
/// A draft that already ends in a space is left alone; a draft that does not
/// gets one, because "cargo buildcargo test" is not what either was said.
#[must_use]
pub fn glued(base: &str, spoken: &str) -> String {
    if spoken.is_empty() {
        return base.to_owned();
    }
    if base.is_empty() || base.ends_with(' ') {
        format!("{base}{spoken}")
    } else {
        format!("{base} {spoken}")
    }
}

/// One painted draft, and where the untrusted part begins.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaintedDraft {
    /// The field's value: the draft with only the settled words appended.
    pub text: String,
    /// The character index the provisional tail starts at, or `None` when there
    /// is nothing provisional to paint.
    pub provisional_from: Option<usize>,
}

impl PaintedDraft {
    /// The settled head the ghost mirror draws behind the hypothesis.
    #[must_use]
    pub fn settled_head(&self) -> &str {
        match self.provisional_from {
            Some(from) => &self.text[..from],
            None => &self.text,
        }
    }

    /// The provisional tail, which is what the ghost paints.
    #[must_use]
    pub fn provisional_tail(&self) -> &str {
        match self.provisional_from {
            Some(from) => &self.text[from..],
            None => "",
        }
    }

    /// The draft as it may be STORED.
    ///
    /// An unproven hypothesis is the mirror's, not the operator's: a stored
    /// draft comes back as ordinary text after a pane switch, a compact swap
    /// or a reload, and a guess that was never spoken must not come back as
    /// one. A boundary that no longer describes this text — the field was
    /// cleared or replaced under it — is ignored rather than obeyed, so a typed
    /// draft is never truncated by a mark left behind.
    #[must_use]
    pub fn persisted(&self) -> String {
        match self.provisional_from {
            Some(from) => self
                .text
                .get(..from)
                .map_or_else(|| self.text.clone(), |head| head.trim_end().to_owned()),
            None => self.text.clone(),
        }
    }

    /// Whether the ghost mirror should be in the DOM at all.
    #[must_use]
    pub fn has_ghost(&self) -> bool {
        self.provisional_from.is_some()
    }
}

/// Paint an interim transcript onto a draft.
#[must_use]
pub fn paint(base: &str, update: &LiveTranscript) -> PaintedDraft {
    let spoken = match (update.settled.is_empty(), update.hypothesis.is_empty()) {
        (false, false) => format!("{} {}", update.settled, update.hypothesis),
        (false, true) => update.settled.clone(),
        (true, _) => update.hypothesis.clone(),
    };
    if spoken.is_empty() {
        return PaintedDraft {
            text: base.to_owned(),
            provisional_from: None,
        };
    }
    let text = glued(base, &spoken);
    let provisional_from = if update.hypothesis.is_empty() {
        None
    } else {
        Some(text.len() - update.hypothesis.len())
    };
    PaintedDraft {
        text,
        provisional_from,
    }
}

/// The value the field keeps when a recording ends without a final result: the
/// settled words alone, still glued onto the draft the recording started from.
#[must_use]
pub fn settled_only(base: &str, settled: &str) -> String {
    glued(base, settled.trim())
}

#[cfg(test)]
mod tests {
    use super::{LiveTranscript, PaintedDraft, glued, paint, settled_only};

    fn update(settled: &str, hypothesis: &str) -> LiveTranscript {
        LiveTranscript {
            settled: settled.to_owned(),
            hypothesis: hypothesis.to_owned(),
        }
    }

    #[test]
    fn speech_is_glued_onto_a_draft_that_has_no_trailing_space() {
        assert_eq!(glued("cargo build", "cargo test"), "cargo build cargo test");
        assert_eq!(
            glued("cargo build ", "cargo test"),
            "cargo build cargo test"
        );
        assert_eq!(glued("", "hello"), "hello");
    }

    #[test]
    fn nothing_spoken_changes_nothing() {
        assert_eq!(glued("draft", ""), "draft");
        let painted = paint("draft", &update("", ""));
        assert_eq!(painted.text, "draft");
        assert_eq!(painted.provisional_from, None);
        assert!(!painted.has_ghost());
    }

    #[test]
    fn a_hypothesis_never_reaches_the_field_value() {
        let painted = paint("typed base still speaking", &update("", "PCM ready"));
        assert_eq!(painted.text, "typed base still speaking PCM ready");
        assert_eq!(painted.provisional_tail(), "PCM ready");
        assert_eq!(painted.settled_head(), "typed base still speaking ");
        assert!(painted.has_ghost());
    }

    #[test]
    fn settled_words_move_into_the_value_and_the_tail_follows_them() {
        let painted = paint("typed base", &update("finished speech", "still recording"));
        assert_eq!(painted.text, "typed base finished speech still recording");
        assert_eq!(painted.settled_head(), "typed base finished speech ");
        assert_eq!(painted.provisional_tail(), "still recording");
    }

    #[test]
    fn a_finalized_result_drops_the_ghost_entirely() {
        let painted = paint("typed base", &update("hello from the mic", ""));
        assert_eq!(painted.text, "typed base hello from the mic");
        assert_eq!(painted.provisional_from, None);
        assert_eq!(painted.provisional_tail(), "");
        assert!(!painted.has_ghost());
    }

    #[test]
    fn the_settled_head_is_the_whole_value_when_nothing_is_provisional() {
        let painted = PaintedDraft {
            text: "draft".to_owned(),
            provisional_from: None,
        };
        assert_eq!(painted.settled_head(), "draft");
    }

    #[test]
    fn a_recording_that_ends_without_a_result_keeps_only_its_settled_words() {
        assert_eq!(
            settled_only("typed base", " finished speech "),
            "typed base finished speech"
        );
        assert_eq!(settled_only("typed base", ""), "typed base");
    }

    #[test]
    fn a_replaced_hypothesis_moves_the_tail_boundary_not_the_value() {
        let first = paint("typed base", &update("", "PCM"));
        let second = paint("typed base", &update("", "PCM ready"));
        assert_eq!(first.text, "typed base PCM");
        assert_eq!(second.text, "typed base PCM ready");
        assert_eq!(second.provisional_from.unwrap(), "typed base ".len());
        assert_eq!(second.provisional_tail(), "PCM ready");
    }

    #[test]
    fn an_ordinary_draft_is_stored_exactly_as_it_was_typed() {
        let painted = PaintedDraft {
            text: "cargo build ".to_owned(),
            provisional_from: None,
        };
        assert_eq!(painted.persisted(), "cargo build ");
        assert!(
            !painted.has_ghost(),
            "a draft nobody is speaking into has nothing provisional to store"
        );
    }

    #[test]
    fn only_the_unspoken_tail_is_left_out_of_the_stored_draft() {
        let painted = paint("cargo build", &update("", "still recording"));
        assert_eq!(
            painted.persisted(),
            "cargo build",
            "the glue that separates the draft from the guess is not part of it"
        );
    }

    #[test]
    fn a_boundary_left_over_from_another_value_is_ignored_rather_than_obeyed() {
        // The mark is set by the paint that ran last; a field that was cleared
        // under it is shorter than the mark, and slicing at it would panic or
        // truncate the operator's own text.
        let painted = PaintedDraft {
            text: "cargo".to_owned(),
            provisional_from: Some("cargo build still recording".len()),
        };
        assert_eq!(painted.persisted(), "cargo");
        let misaligned = PaintedDraft {
            text: "héllo".to_owned(),
            provisional_from: Some(2),
        };
        assert_eq!(misaligned.persisted(), "héllo");
    }

    #[test]
    fn storing_the_head_leaves_the_field_showing_the_whole_hypothesis() {
        // The tail is what the ghost mirror is for; only the STORE drops it, so
        // the field an operator is reading is unchanged by what is persisted.
        let painted = paint("cargo build", &update("", "still recording"));
        assert_eq!(painted.text, "cargo build still recording");
        assert_eq!(painted.provisional_tail(), "still recording");
        assert_eq!(painted.persisted(), "cargo build");
    }
}
