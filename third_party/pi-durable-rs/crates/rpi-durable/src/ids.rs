//! Branded record identities and commit sequences (port of `src/ids.ts` @ `abe508e1b`).

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Erased nominal number identifying one durable record kind (`Id<Kind>`).
///
/// Upstream brands are compile-time only; the numeric value is the storage
/// identity. `T` is a zero-sized marker (`Conversation`, `Entry`, ...).
pub struct Id<T> {
    value: u64,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    pub const fn from_number(value: u64) -> Self {
        Self {
            value,
            _marker: PhantomData,
        }
    }

    pub const fn as_number(self) -> u64 {
        self.value
    }

    pub const fn get(self) -> u64 {
        self.value
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}
impl<T> std::hash::Hash for Id<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}
impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl<T> Serialize for Id<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.value)
    }
}

impl<'de, T> Deserialize<'de> for Id<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_number(u64::deserialize(deserializer)?))
    }
}

/// Strictly increasing sequence assigned to one atomic storage commit; gaps
/// are permitted (port of `Seq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    pub const fn from_number(value: u64) -> Self {
        Self(value)
    }
    pub const fn as_number(self) -> u64 {
        self.0
    }
}
impl From<u64> for Seq {
    fn from(value: u64) -> Self {
        Self(value)
    }
}
impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Marker for conversation identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversation {}
/// Marker for entry identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {}
/// Marker for task identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {}
/// Marker for submission identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {}
/// Marker for document identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Document {}

pub type ConversationId = Id<Conversation>;
pub type EntryId = Id<Entry>;
pub type TaskId = Id<Task>;
pub type SubmissionId = Id<Submission>;
pub type DocumentId = Id<Document>;

/// The immutable root conversation pre-allocated at identity 1.
pub const ROOT_CONVERSATION_ID: ConversationId = ConversationId::from_number(1);

/// Apply an erased ID brand at a trusted numeric allocation or decoding boundary.
pub fn id_from_number<T>(value: u64) -> Id<T> {
    Id::from_number(value)
}

/// Apply the erased commit-sequence brand at a trusted storage boundary.
pub fn seq_from_number(value: u64) -> Seq {
    Seq(value)
}
