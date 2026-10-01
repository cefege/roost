//! The browser half of the keyterm lexicon: what is remembered, and how a
//! session's terms are learned into it.
//!
//! The arithmetic lives in `super::keyterm_lexicon` and is pure; this is the
//! storage seam around it. A tab that cannot read storage still records: the
//! lexicon is a bias, and a missing bias must never cost a recording.
//! Ports the storage half of `apps/web/src/voice/keytermLexicon.ts`.

use super::keyterm_lexicon::{self, Lexicon, STORAGE_KEY, TOP_TERMS};

/// What this browser remembers, or nothing when storage is unavailable or holds
/// something this code does not understand.
#[must_use]
pub fn remembered() -> Lexicon {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten())
            .map_or_else(Lexicon::new, |stored| keyterm_lexicon::parse(&stored))
    }
    #[cfg(not(target_arch = "wasm32"))]
    Lexicon::new()
}

/// Learn a session's terms, and return the vocabulary to seed the next one with.
///
/// Learned from the terms the SCREEN produced, before the lexicon is seeded into
/// them: a term this browser already remembers must not be able to vote for
/// itself, or yesterday's terms would outrank what the operator said today.
#[must_use]
pub fn learn(screen_terms: Vec<String>) -> Vec<String> {
    let previous = remembered();
    let merged = keyterm_lexicon::merge(&previous, &screen_terms);
    store(&merged);
    keyterm_lexicon::top_terms(&merged, TOP_TERMS)
}

#[cfg(target_arch = "wasm32")]
fn store(lexicon: &Lexicon) {
    let Ok(stored) = serde_json::to_string(lexicon) else {
        return;
    };
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(STORAGE_KEY, &stored);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn store(_lexicon: &Lexicon) {}

#[cfg(test)]
mod tests {
    use super::learn;
    use crate::voice::keyterm_lexicon::Lexicon;

    #[test]
    fn a_tab_with_no_storage_still_learns_and_returns_the_session_vocabulary() {
        // The host build has no localStorage, which is the same shape as a
        // browser that refused it: the caller must still get a vocabulary.
        assert_eq!(super::remembered(), Lexicon::new());
        assert_eq!(learn(vec!["kysely".to_owned()]), vec!["kysely".to_owned()]);
    }

    #[test]
    fn an_empty_session_does_not_learn_and_says_so() {
        assert!(learn(Vec::new()).is_empty());
    }
}
