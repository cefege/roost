//! Notification preferences: the six switches that decide how loudly a blocked
//! or finished agent, or a long shell command finishing, gets to interrupt.
//!
//! One JSON object, because that is one preference a user thinks of as one thing,
//! and because a partial write to it must not be possible. The parse is
//! PER-KEY and total: a member this build does not know is dropped, a member
//! whose value is not a boolean keeps its default, and a document that is not
//! JSON at all resolves to the whole default. v2 does the same
//! (`notifyPrefs.ts:35-45`) and the reason is the same one every other preference
//! here follows — a value this build did not write is not a value it should act
//! on.
//!
//! Desktop delivery defaults OFF because enabling it is not a preference: it is
//! an explicit user gesture that has to grant permission and complete the push
//! subscription first, and a flag that says `true` before either has happened is
//! a browser console the user cannot explain.

use std::collections::BTreeMap;

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::NOTIFY_PREFS_KEY;

/// One switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyPref {
    /// Show the notification in the app.
    InApp,
    /// Deliver it through the browser's push service.
    Desktop,
    /// Badge the tab title.
    TitleBadge,
    /// Play a sound when an agent blocks.
    BlockedSound,
    /// Play a sound when an agent finishes.
    DoneSound,
    /// Notify when a long shell command finishes (OSC 133).
    CommandFinished,
}

impl NotifyPref {
    /// The stored member name, which is also the key in the JSON object.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InApp => "inApp",
            Self::Desktop => "desktop",
            Self::TitleBadge => "titleBadge",
            Self::BlockedSound => "blockedSound",
            Self::DoneSound => "doneSound",
            Self::CommandFinished => "commandFinished",
        }
    }

    /// Every switch, in the order the defaults are written.
    pub const ALL: [Self; 6] = [
        Self::InApp,
        Self::Desktop,
        Self::TitleBadge,
        Self::BlockedSound,
        Self::DoneSound,
        Self::CommandFinished,
    ];
}

/// The six values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyPrefs {
    /// Show the notification in the app.
    pub in_app: bool,
    /// Deliver it through the browser's push service.
    pub desktop: bool,
    /// Badge the tab title.
    pub title_badge: bool,
    /// Play a sound when an agent blocks.
    pub blocked_sound: bool,
    /// Play a sound when an agent finishes.
    pub done_sound: bool,
    /// Notify when a long shell command finishes.
    pub command_finished: bool,
}

impl Default for NotifyPrefs {
    fn default() -> Self {
        Self {
            in_app: true,
            desktop: false,
            title_badge: true,
            blocked_sound: false,
            done_sound: false,
            command_finished: true,
        }
    }
}

impl NotifyPrefs {
    /// One switch's value.
    pub fn get(&self, pref: NotifyPref) -> bool {
        match pref {
            NotifyPref::InApp => self.in_app,
            NotifyPref::Desktop => self.desktop,
            NotifyPref::TitleBadge => self.title_badge,
            NotifyPref::BlockedSound => self.blocked_sound,
            NotifyPref::DoneSound => self.done_sound,
            NotifyPref::CommandFinished => self.command_finished,
        }
    }

    fn set(&mut self, pref: NotifyPref, value: bool) {
        match pref {
            NotifyPref::InApp => self.in_app = value,
            NotifyPref::Desktop => self.desktop = value,
            NotifyPref::TitleBadge => self.title_badge = value,
            NotifyPref::BlockedSound => self.blocked_sound = value,
            NotifyPref::DoneSound => self.done_sound = value,
            NotifyPref::CommandFinished => self.command_finished = value,
        }
    }

    /// The object as it is written to storage.
    ///
    /// Named `as_record` rather than `to_record`: the convention clippy
    /// enforces is that a `to_*` taking `&self` is building a NEW value, and
    /// `NotifyPrefs` is `Copy`, so a `to_*` on it reads as a conversion of a
    /// borrowed temporary rather than a projection of a value the caller still
    /// owns.
    fn as_record(&self) -> BTreeMap<&'static str, bool> {
        let mut record = BTreeMap::new();
        for pref in NotifyPref::ALL {
            record.insert(pref.as_str(), self.get(pref));
        }
        record
    }
}

/// Read the stored object, or the defaults when it cannot be read.
pub fn parse(raw: Option<&str>) -> NotifyPrefs {
    let Some(raw) = raw else {
        return NotifyPrefs::default();
    };
    let Ok(document) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(raw) else {
        return NotifyPrefs::default();
    };
    let mut prefs = NotifyPrefs::default();
    for pref in NotifyPref::ALL {
        // Per KEY, not per document: one member of the wrong type costs that
        // member its stored value and nothing else.
        if let Some(value) = document
            .get(pref.as_str())
            .and_then(serde_json::Value::as_bool)
        {
            prefs.set(pref, value);
        }
    }
    prefs
}

/// Serialise the object. A `BTreeMap` of the five known members, so a member
/// added by a later build is not written by this one.
fn serialise(prefs: &NotifyPrefs) -> String {
    serde_json::to_string(&prefs.as_record()).unwrap_or_else(|_| "{}".to_owned())
}

/// Change one switch, and persist.
pub fn set_notify_pref(
    store: &mut Store,
    storage: &dyn KeyValueStore,
    pref: NotifyPref,
    value: bool,
) -> bool {
    if store.prefs.notify.get(pref) == value {
        return false;
    }
    store.prefs.notify.set(pref, value);
    storage.set(NOTIFY_PREFS_KEY, &serialise(&store.prefs.notify));
    store.note_change();
    tracing::debug!(target: "store", pref = pref.as_str(), value, "notification pref");
    true
}

/// Record that desktop delivery is now genuinely available.
///
/// The HOST calls this after the permission prompt AND the coordinator
/// subscription have both completed — in that order, because a permission with no
/// subscription delivers nothing and a subscription with no permission asks the
/// browser for nothing. A toggle cannot enforce the order, so it is a named
/// second door rather than a flag a checkbox writes directly.
pub fn confirm_desktop_notifications(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    set_notify_pref(store, storage, NotifyPref::Desktop, true)
}

/// Record that desktop delivery has been turned off, before the host
/// unsubscribes — so a host that fails to unsubscribe still stops claiming the
/// browser is subscribed.
pub fn revoke_desktop_notifications(store: &mut Store, storage: &dyn KeyValueStore) -> bool {
    set_notify_pref(store, storage, NotifyPref::Desktop, false)
}
