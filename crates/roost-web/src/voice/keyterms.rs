//! Which on-screen words are worth biasing the recognizer toward.
//!
//! A terminal is full of words the acoustic model has never heard — crate
//! names, machine names, a project's own jargon — and the recognizer's usual
//! failure is not silence but a confident wrong word. Scoring the visible grid,
//! the recent scrollback and the operator's own typing costs nothing at open
//! time and fixes the most common of those.
//! Ports `apps/web/src/voice/keytermContext.ts`, with the stop set in
//! `super::keyterm_stopwords`.

use std::collections::HashMap;
use std::rc::Rc;

/// The upper bound on a keyterm's token count. Deepgram's own guidance, and past
/// it a "term" is a sentence that will not match a spoken word.
pub const MAX_KEYTERM_TOKENS: usize = 250;

/// The upper bound on keyterms in one connection.
pub const MAX_KEYTERM_ENTRIES: usize = 80;

/// The scored candidates kept before the speakable and budget bounds apply.
pub const MAX_TERMS: usize = 90;

/// The byte budget for the encoded term plus its spoken variant.
pub const MAX_KEYTERM_URL_BYTES: usize = 4000;

/// A scrollback row two hundred lines back still counts, at this fraction. The
/// vocabulary of a session is recent, so recency is weighted but not decisive.
pub const SCROLLBACK_DECAY_FLOOR: f64 = 0.3;

/// What the operator typed themselves: the strongest signal there is, because
/// they spelled it.
pub const INPUT_WEIGHT: f64 = 2.0;

/// Persisted jargon: strong enough to seed a cold session, weaker than typing.
pub const LEXICON_WEIGHT: f64 = 1.5;

/// What the engine is given: the term as it appeared, and the phrase to hear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyterm {
    /// The token, as it appeared on screen.
    pub term: String,
    /// The spoken form, when it differs from the term.
    pub variant: String,
}

/// The live terminal context one recording is scored against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalContext {
    /// The visible viewport text.
    pub grid: String,
    /// Recent history, newest last.
    pub scrollback: String,
    /// What the operator has typed recently.
    pub input: String,
    /// Jargon learned in earlier sessions, best first.
    pub lexicon: Vec<String>,
}

/// A live reader the engine asks for vocabulary when a socket opens.
///
/// A prop type of its own because a component prop must be comparable and a
/// closure is not: two readers are the same reader when they are the same
/// allocation, which is what a component needs to know about a prop it did not
/// change.
#[derive(Clone)]
pub struct ContextReader(Rc<dyn Fn() -> TerminalContext>);

impl ContextReader {
    /// Wrap a reader.
    #[must_use]
    pub fn new(read: impl Fn() -> TerminalContext + 'static) -> Self {
        Self(Rc::new(read))
    }

    /// A reader for a composer with no terminal to read.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(TerminalContext::default)
    }

    /// Read the context now.
    #[must_use]
    pub fn call(&self) -> TerminalContext {
        (self.0)()
    }

    /// The reader, for a host that asks it outside a component.
    #[must_use]
    pub fn into_source(self) -> Rc<dyn Fn() -> TerminalContext> {
        self.0
    }
}

impl std::fmt::Debug for ContextReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContextReader")
    }
}

impl PartialEq for ContextReader {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// A scored candidate: the running score, and the surface form first seen.
#[derive(Debug, Clone, Default)]
struct Candidate {
    score: f64,
    surface: String,
}

type Candidates = HashMap<String, Candidate>;

// The word-level rules live in `keyterm_forms`, and are re-exported here so a
// caller decides vocabulary in one place.
use super::keyterm_forms::{is_capitalised_word, tokens};
pub use super::keyterm_forms::{
    is_speakable, keep, normalize, spoken_form, structural_bonus, token_count,
};

/// Score the live context, then decide the vocabulary to send.
#[must_use]
pub fn extract(context: &TerminalContext) -> Vec<Keyterm> {
    let mut candidates = Candidates::new();
    score_tokens(&context.grid, 1.0, &mut candidates);
    let rows: Vec<&str> = context.scrollback.lines().collect();
    for (index, row) in rows.iter().enumerate() {
        let recency = if rows.len() <= 1 {
            1.0
        } else {
            SCROLLBACK_DECAY_FLOOR
                + (1.0 - SCROLLBACK_DECAY_FLOOR) * (index as f64 / (rows.len() - 1) as f64)
        };
        score_tokens(row, recency, &mut candidates);
    }
    score_tokens(&context.input, INPUT_WEIGHT, &mut candidates);
    mine_phrases(&context.grid, 2.0, &mut candidates);
    mine_phrases(&context.input, INPUT_WEIGHT * 1.5, &mut candidates);
    finalize(&candidates, &context.lexicon)
}

/// Score one text source, counting each distinct token once per source so a word
/// repeated down a log does not outrank one the operator just typed.
fn score_tokens(text: &str, weight: f64, candidates: &mut Candidates) {
    let mut counted: Vec<String> = Vec::new();
    for token in tokens(text) {
        let normalized = normalize(&token);
        if !keep(&normalized) || counted.contains(&normalized) {
            continue;
        }
        counted.push(normalized.clone());
        let bonus = structural_bonus(&normalized);
        let entry = candidates.entry(normalized).or_insert_with(|| Candidate {
            score: 0.0,
            surface: token,
        });
        entry.score += weight * bonus;
    }
}

/// Capitalised word pairs, which are the project's own product names and are
/// never guessed from single tokens.
fn mine_phrases(text: &str, weight: f64, candidates: &mut Candidates) {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_ascii_alphabetic())
                .to_owned()
        })
        .filter(|word| !word.is_empty())
        .collect();
    for window in words.windows(2) {
        if !is_capitalised_word(&window[0]) || !is_capitalised_word(&window[1]) {
            continue;
        }
        let phrase = format!("{} {}", window[0], window[1]);
        let entry = candidates
            .entry(phrase.to_lowercase())
            .or_insert_with(|| Candidate {
                score: 0.0,
                surface: phrase.clone(),
            });
        entry.score += weight;
    }
}

/// Rank the scored tokens and turn the best of them into keyterms.
fn finalize(candidates: &Candidates, lexicon: &[String]) -> Vec<Keyterm> {
    let mut ranked: Vec<(String, Candidate)> = candidates
        .iter()
        .map(|(token, candidate)| (token.clone(), candidate.clone()))
        .collect();
    for term in lexicon {
        let key = term.to_lowercase();
        if !candidates.contains_key(&key) {
            ranked.push((
                key,
                Candidate {
                    score: LEXICON_WEIGHT,
                    surface: term.clone(),
                },
            ));
        }
    }
    ranked.sort_by(|left, right| {
        right
            .1
            .score
            .total_cmp(&left.1.score)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(MAX_TERMS);

    let mut keyterms: Vec<Keyterm> = Vec::new();
    let mut tokens_spent = 0usize;
    let mut bytes_spent = 0usize;
    for (token, candidate) in ranked {
        if !keep(&token)
            || keyterms
                .iter()
                .any(|existing| existing.term == candidate.surface)
        {
            continue;
        }
        let variant = spoken_form(&token).unwrap_or_else(|| candidate.surface.clone());
        if !is_speakable(&variant) {
            continue;
        }
        let cost = 8 + super::handshake::uri_encode(&variant).len();
        let weight = token_count(&token);
        if tokens_spent + weight > MAX_KEYTERM_TOKENS || bytes_spent + cost > MAX_KEYTERM_URL_BYTES
        {
            continue;
        }
        tokens_spent += weight;
        bytes_spent += cost;
        keyterms.push(Keyterm {
            term: candidate.surface.clone(),
            variant,
        });
        if keyterms.len() >= MAX_KEYTERM_ENTRIES {
            break;
        }
    }
    keyterms
}

#[cfg(test)]
mod tests;
