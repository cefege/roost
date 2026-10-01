//! The two things a recording needs from outside itself: the coordinator's
//! stored key, and the vocabulary to bias the recognizer with.
//!
//! Both are built here rather than in the component because both outlive the
//! call that asked for them: the engine holds the key source across a reconnect,
//! and holds the keyterm source until the socket closes. The key is cached for
//! the page's life, because a key that has not changed should not cost a round
//! trip on every tap.

use std::rc::Rc;

use crate::pump::Pump;

use super::deepgram_engine::{GrantSource, KeytermSource};
use super::keyterms::{Keyterm, TerminalContext};

/// How many remembered terms ride one connection.
pub const LEXICON_TOP_TERMS: usize = 40;

/// The credential source: the coordinator's stored key, once per page.
#[must_use]
pub fn grant_source(pump: Pump) -> GrantSource {
    Rc::new(move || {
        let pump = pump.clone();
        Box::pin(async move {
            use roost_client_core::client::rpc::calls::settings::transcription::GrantTranscriptionToken;
            pump.rpc()
                .call(&GrantTranscriptionToken)
                .await
                .map_err(|error| error.to_string())
        })
    })
}

/// The keyterm source for one composer: the words on screen and the words the
/// operator has typed, asked once per socket open because Deepgram fixes the
/// list when the connection opens.
#[must_use]
pub fn keyterm_source(read_context: Rc<dyn Fn() -> TerminalContext>) -> KeytermSource {
    Rc::new(move || {
        let context = read_context();
        // Two passes, in this order: the screen's own vocabulary is learned and
        // stored FIRST, from terms that carry no seed; then the remembered
        // vocabulary is seeded in. Seeding first would let yesterday's terms
        // inflate today's ranking, and the term that wins would be the one this
        // browser already believed.
        let learned: Vec<String> = super::keyterms::extract(&context)
            .into_iter()
            .take(LEXICON_TOP_TERMS)
            .map(|keyterm: Keyterm| keyterm.term)
            .collect();
        let lexicon = super::lexicon_store::learn(learned.clone());
        super::keyterms::extract(&TerminalContext { lexicon, ..context })
            .into_iter()
            .take(LEXICON_TOP_TERMS)
            .collect()
    })
}
