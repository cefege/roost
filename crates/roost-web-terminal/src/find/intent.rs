//! One-shot handoff from fleet-wide content search to pane-local terminal
//! find. A mounted pane consumes an intent immediately; a cold pane consumes the
//! latest one on registration. A credential boundary clears both the mounted
//! callbacks and the pending session identities.
//!
//! The registry is a VALUE, not a module singleton: v2 held it in a module-level
//! `Map` that only a credential reset emptied, which is exactly the shape that
//! leaks one credential's session ids into the next. Here the owner is whoever
//! holds the registry, so a boundary is `reset()` on a value the boundary owns.
//!
//! Ported from `apps/web/src/renderer/terminalFindIntent.ts`.

use std::collections::BTreeMap;

use crate::find::hits::{FindQueryOptions, PreferredMatch};

/// What global search asked a pane to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFindIntent {
    /// The literal to search for, never a pattern.
    pub literal_query: String,
    /// Whether the pane should search case-sensitively.
    pub case_sensitive: bool,
    /// The coordinator coordinate to activate, when the caller had one.
    pub preferred_global_match: Option<PreferredMatch>,
}

/// What a caller may add to an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalFindIntentOptions {
    /// The case sensitivity to hand the pane; absent means insensitive.
    pub case_sensitive: Option<bool>,
    /// The coordinator coordinate to hand the pane.
    pub preferred_global_match: Option<PreferredMatch>,
}

impl TerminalFindIntentOptions {
    /// The intent these options describe for `literal_query`.
    pub fn intent(self, literal_query: &str) -> TerminalFindIntent {
        TerminalFindIntent {
            literal_query: literal_query.to_string(),
            case_sensitive: self.case_sensitive.unwrap_or(false),
            preferred_global_match: self.preferred_global_match,
        }
    }
}

/// The query options an applied intent becomes.
///
/// The conversion is one place because it is one rule: an intent is a LITERAL
/// from a result list, so a pane in regex mode has to be reset before the needle
/// is searched or it searches for a pattern nobody typed.
impl TerminalFindIntent {
    /// The options a pane's `set_query` is called with.
    pub fn query_options(&self) -> FindQueryOptions {
        FindQueryOptions {
            literal: true,
            case_sensitive: Some(self.case_sensitive),
            preferred_match: self.preferred_global_match.clone(),
        }
    }
}

/// A mounted pane a find intent can be applied to.
pub trait FindIntentSink {
    /// Show the find bar.
    fn open_find(&mut self);
    /// Search `query` under `options`.
    fn set_query(&mut self, query: &str, options: FindQueryOptions);
}

/// Per-session intents, and the pane each is waiting for.
#[derive(Debug, Default)]
pub struct FindIntentRegistry {
    registrations: BTreeMap<String, (u64, Box<dyn FindIntentSink>)>,
    pending: BTreeMap<String, TerminalFindIntent>,
    next_registration: u64,
}

impl FindIntentRegistry {
    /// A registry with no mounted panes and nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a pane, consuming its pending intent if one is waiting.
    ///
    /// Returns the registration id to pass back to `unregister`. A disposer names
    /// the registration it was ISSUED for, so a pane that unmounts late cannot
    /// unregister the pane that replaced it: the id is what keeps a stale
    /// disposer from silencing a live pane.
    pub fn register(
        &mut self,
        session_id: &str,
        sink: Box<dyn FindIntentSink>,
    ) -> u64 {
        self.next_registration += 1;
        let id = self.next_registration;
        self.registrations
            .insert(session_id.to_string(), (id, sink));
        if let Some(intent) = self.pending.remove(session_id) {
            self.apply(session_id, &intent);
        }
        id
    }

    /// Unmount the pane holding `registration`, and only that one.
    pub fn unregister(&mut self, session_id: &str, registration: u64) {
        if self
            .registrations
            .get(session_id)
            .is_some_and(|(held, _)| *held == registration)
        {
            self.registrations.remove(session_id);
        }
    }

    /// Hand an intent to a session's pane, or hold it until one mounts.
    pub fn request(
        &mut self,
        session_id: &str,
        literal_query: &str,
        options: TerminalFindIntentOptions,
    ) {
        let intent = options.intent(literal_query);
        if self.registrations.contains_key(session_id) {
            self.apply(session_id, &intent);
            return;
        }
        self.pending.insert(session_id.to_string(), intent);
    }

    /// Whether a session's pane is mounted.
    pub fn is_mounted(&self, session_id: &str) -> bool {
        self.registrations.contains_key(session_id)
    }

    /// The intent still waiting for a session's pane, if any.
    pub fn pending_for(&self, session_id: &str) -> Option<&TerminalFindIntent> {
        self.pending.get(session_id)
    }

    /// Drop every mounted callback and every pending session identity.
    ///
    /// This is the credential boundary. A session id from the previous credential
    /// must not survive into the next one, because a pane mounting afterwards
    /// would consume a query the new credential never issued.
    pub fn reset(&mut self) {
        self.registrations.clear();
        self.pending.clear();
    }

    /// Open the bar and search the literal, through the pane that holds it.
    fn apply(&mut self, session_id: &str, intent: &TerminalFindIntent) {
        let Some((_, sink)) = self.registrations.get_mut(session_id) else {
            return;
        };
        sink.open_find();
        sink.set_query(&intent.literal_query, intent.query_options());
    }
}
