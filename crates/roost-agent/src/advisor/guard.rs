//! Ported from oh-my-pi packages/coding-agent/src/advisor/emission-guard.ts (MIT).
//! Normalizes advice, suppresses noise and lower-severity repeats, and bounds
//! non-blocking emissions within a single review.

use std::collections::{HashMap, VecDeque};

use unicode_normalization::UnicodeNormalization;

use crate::records::AdvisorySeverity;

const HISTORY_LIMIT: usize = 4096;
const NON_BLOCKER_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Emission {
    Accepted,
    Duplicate,
    Noise,
    Budget,
}

impl Emission {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Noise => "noise",
            Self::Budget => "budget",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct EmissionGuard {
    history: HashMap<String, AdvisorySeverity>,
    order: VecDeque<String>,
    non_blockers: usize,
}

impl EmissionGuard {
    pub(crate) fn begin_review(&mut self) {
        self.non_blockers = 0;
    }

    pub(crate) fn check(&mut self, note: &str, severity: AdvisorySeverity) -> Emission {
        let key = normalize(note);
        if key.is_empty() || is_noise(&key) {
            return Emission::Noise;
        }
        if severity != AdvisorySeverity::Blocker && self.non_blockers >= NON_BLOCKER_LIMIT {
            return Emission::Budget;
        }
        if self
            .history
            .get(&key)
            .is_some_and(|previous| severity <= *previous)
        {
            return Emission::Duplicate;
        }
        if self.history.contains_key(&key) {
            self.order.retain(|existing| existing != &key);
        }
        self.history.insert(key.clone(), severity);
        self.order.push_back(key);
        if self.order.len() > HISTORY_LIMIT
            && let Some(oldest) = self.order.pop_front()
        {
            self.history.remove(&oldest);
        }
        if severity != AdvisorySeverity::Blocker {
            self.non_blockers += 1;
        }
        Emission::Accepted
    }
}

pub(crate) fn normalize(note: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for character in note.nfkc().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separator && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    normalized
}

fn is_noise(note: &str) -> bool {
    matches!(
        note,
        "stop" | "done" | "complete" | "no issue continue" | "lgtm" | "nothing to add"
    )
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalization_nfkc_lowercases_and_collapses_punctuation() {
        assert_eq!(normalize("  ＨＥＬＬＯ—World!! "), "hello world");
    }
}
