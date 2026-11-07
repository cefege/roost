//! Port of `packages/agent/src/harness/session/memory-repo.ts` @ pi 0.82.1 (2efa728) —
//! in-memory `SessionRepo` implementation.
//!
//! Intentional differences:
//! - `create` / `open` / `list` / `delete` implement the [`SessionRepo`] trait
//!   (types.rs:1030-1058) — the upstream class API maps onto the trait surface.
//! - `fork` options combine `SessionForkOptions` and `SessionCreateOptions` (upstream
//!   `SessionForkOptions & { id?: string }`, memory-repo.ts:36): the trait's `fork`
//!   takes `TCreateOptions` only (types.rs:1023-1029), which cannot carry `entryId` /
//!   `position`, so the full-options variant is an inherent method and the trait
//!   implementation delegates with `entry_id: None` (full copy).
//! - Sessions are kept in a `BTreeMap` keyed by id. Upstream uses a `Map` (insertion
//!   order); ids sort deterministically here — `list` order is unspecified (upstream
//!   tests only assert single-element lists).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::harness::types::{
    ForkPosition, Session, SessionCreateOptions, SessionError, SessionErrorCode, SessionMetadata,
    SessionRepo,
};

use super::memory_storage::{InMemorySessionStorage, InMemorySessionStorageOptions};
use super::repo_utils::{create_session_id, create_timestamp, get_entries_to_fork, to_session};

/// `InMemorySessionRepo` (memory-repo.ts:5-49).
///
/// The session registry lives behind a `Mutex` because the [`SessionRepo`] trait only
/// exposes `&self` methods (upstream is a mutable class field).
pub struct InMemorySessionRepo {
    sessions: Mutex<BTreeMap<String, Arc<dyn Session<Metadata = SessionMetadata>>>>,
}

/// Fork options — upstream `SessionForkOptions & { id?: string }` (memory-repo.ts:36).
#[derive(Debug, Clone, Default)]
pub struct MemorySessionForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
    pub id: Option<String>,
}

impl Default for InMemorySessionRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySessionRepo {
    /// Constructor.
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    /// Full-options `fork` (memory-repo.ts:35-49).
    pub async fn fork(
        &self,
        source: SessionMetadata,
        options: MemorySessionForkOptions,
    ) -> Result<Arc<dyn Session<Metadata = SessionMetadata>>, SessionError> {
        let source_session = self.open(source).await?;
        let forked_entries = get_entries_to_fork(
            source_session.storage().as_ref(),
            options.entry_id.as_deref(),
            options.position,
        )
        .await?;
        let metadata = SessionMetadata {
            id: options.id.unwrap_or_else(create_session_id),
            created_at: create_timestamp(),
        };
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: Some(forked_entries),
            metadata: Some(metadata.clone()),
        })?;
        let session = to_session(Arc::new(storage));
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(metadata.id, Arc::clone(&session));
        Ok(session)
    }
}

#[async_trait]
impl SessionRepo<SessionMetadata, SessionCreateOptions> for InMemorySessionRepo {
    /// `create` (memory-repo.ts:8-17).
    async fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = SessionMetadata>>, SessionError> {
        let metadata = SessionMetadata {
            id: options.id.unwrap_or_else(create_session_id),
            created_at: create_timestamp(),
        };
        let storage = InMemorySessionStorage::new(InMemorySessionStorageOptions {
            entries: None,
            metadata: Some(metadata.clone()),
        })?;
        let session = to_session(Arc::new(storage));
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(metadata.id, Arc::clone(&session));
        Ok(session)
    }

    /// `open` (memory-repo.ts:19-25).
    async fn open(
        &self,
        metadata: SessionMetadata,
    ) -> Result<Arc<dyn Session<Metadata = SessionMetadata>>, SessionError> {
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&metadata.id)
            .cloned();
        match session {
            Some(session) => Ok(session),
            None => Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Session not found: {}", metadata.id),
            )),
        }
    }

    /// `list` (memory-repo.ts:27-29).
    async fn list(&self, _options: ()) -> Result<Vec<SessionMetadata>, SessionError> {
        let sessions: Vec<Arc<dyn Session<Metadata = SessionMetadata>>> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();
        let mut metadata = Vec::with_capacity(sessions.len());
        for session in sessions {
            metadata.push(session.get_metadata().await?);
        }
        Ok(metadata)
    }

    /// `delete` (memory-repo.ts:31-33).
    async fn delete(&self, metadata: SessionMetadata) -> Result<(), SessionError> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&metadata.id);
        Ok(())
    }

    /// Trait surface `fork`: no `entryId` / `position` (types.rs:1023-1029) — full
    /// copy. See the inherent [`InMemorySessionRepo::fork`] for the upstream options.
    async fn fork(
        &self,
        source: SessionMetadata,
        options: SessionCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = SessionMetadata>>, SessionError> {
        self.fork(
            source,
            MemorySessionForkOptions {
                entry_id: None,
                position: None,
                id: options.id,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    /// `expect_err` without a `T: Debug` bound (the storage types are not `Debug`).
    fn expect_err<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: expected an error"),
            Err(error) => error,
        }
    }
    use crate::harness::session::repo_utils::test_support::user_message;
    use crate::harness::types::{SessionEntryCursorOptions, SessionErrorCode, SessionRepo};

    use super::*;

    #[tokio::test]
    async fn test_create_open_list_delete_lifecycle() {
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(SessionCreateOptions {
                id: Some("session-1".to_owned()),
            })
            .await
            .expect("create");
        let metadata = session.get_metadata().await.expect("metadata");
        assert_eq!(metadata.id, "session-1");
        // open returns the same session instance (`repo.open(metadata) === session`,
        // repo.test.ts:16).
        let opened = repo.open(metadata.clone()).await.expect("open");
        assert!(Arc::ptr_eq(&session, &opened));
        let listed = repo.list(()).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "session-1");
        repo.delete(metadata.clone()).await.expect("delete");
        let error = expect_err(repo.open(metadata).await, "open after delete");
        assert_eq!(error.code, SessionErrorCode::NotFound);
        assert!(error.message.contains("Session not found: session-1"));
    }

    #[tokio::test]
    async fn test_fork_creates_registered_session_with_new_id() {
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(SessionCreateOptions {
                id: Some("session-1".to_owned()),
            })
            .await
            .expect("create");
        let metadata = session.get_metadata().await.expect("metadata");
        // Populate the source through the `Session` facade (append methods land
        // with the session.rs port; repo-utils.ts `toSession` wraps the storage).
        let user1 = session
            .append_message(user_message("one"))
            .await
            .expect("append");
        let fork = repo
            .fork(
                metadata,
                MemorySessionForkOptions {
                    id: Some("session-2".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("fork");
        let fork_metadata = fork.get_metadata().await.expect("metadata");
        assert_eq!(fork_metadata.id, "session-2");
        // The fork copies the source entries.
        let fork_entries = fork
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries");
        assert_eq!(fork_entries.len(), 1);
        assert_eq!(fork_entries[0].id(), user1.as_str());
        // The fork is registered and openable (repo.test.ts:23 — `open` after
        // `fork` returns the same instance).
        let opened = repo.open(fork_metadata).await.expect("open");
        assert!(Arc::ptr_eq(&fork, &opened));
    }

    #[tokio::test]
    async fn test_fork_rejects_unknown_entry_id() {
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(SessionCreateOptions::default())
            .await
            .expect("create");
        let metadata = session.get_metadata().await.expect("metadata");
        let error = expect_err(
            repo.fork(
                metadata,
                MemorySessionForkOptions {
                    entry_id: Some("missing".to_owned()),
                    ..Default::default()
                },
            )
            .await,
            "fork error",
        );
        assert_eq!(error.code, SessionErrorCode::InvalidForkTarget);
        assert!(error.message.contains("Entry missing not found"));
    }

    #[tokio::test]
    async fn test_fork_trait_surface_copies_full_session() {
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(SessionCreateOptions {
                id: Some("session-1".to_owned()),
            })
            .await
            .expect("create");
        let metadata = session.get_metadata().await.expect("metadata");
        let fork = SessionRepo::fork(
            &repo,
            metadata,
            SessionCreateOptions {
                id: Some("session-2".to_owned()),
            },
        )
        .await
        .expect("fork");
        assert_eq!(fork.get_metadata().await.expect("metadata").id, "session-2");
    }
}
