//! Port of `packages/agent/src/harness/session/jsonl-repo.ts` @ pi 0.82.1 (2efa728) —
//! JSONL-backed session repository.
//!
//! Sessions are stored under `<sessionsRoot>/<encodedCwd>/<timestamp>_<sessionId>.jsonl`
//! where `encodeCwd` is `--` + cwd with the leading `/` (or `\`) stripped and `/`,
//! `\`, `:` replaced by `-`, followed by `--` (jsonl-repo.ts:34-36).
//!
//! Intentional differences:
//! - `create` / `open` / `list` / `delete` implement the [`SessionRepo`] trait
//!   (types.rs:1030-1058). `fork` follows the same split as the memory repo: the
//!   trait's `fork` cannot carry `entryId` / `position` (types.rs:1023-1029), so the
//!   full-options [`JsonlSessionForkOptions`] variant is an inherent method and the
//!   trait implementation delegates with `entry_id: None` (full copy).
//! - The `sessionsRoot` cache (jsonl-repo.ts:48-56) uses a `Mutex<Option<String>>`
//!   because all trait methods take `&self`.
//! - `list` sorts by `createdAt` descending (jsonl-repo.ts:123) via the shared
//!   `parse_iso8601_ms`; unparseable timestamps sort as earliest (upstream `NaN`
//!   comparisons leave them in place — malformed files only).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::harness::types::{
    CreateDirOptions, FileKind, FileSystem, ForkPosition, JsonlSessionCreateOptions,
    JsonlSessionListOptions, JsonlSessionMetadata, RemoveOptions, Session, SessionError,
    SessionErrorCode, SessionRepo, SessionStorage,
};
use crate::session::parse_iso8601_ms;

use super::jsonl_storage::{
    JsonlSessionStorage, JsonlSessionStorageCreateOptions, load_jsonl_session_metadata,
};
use super::repo_utils::{
    create_session_id, create_timestamp, get_entries_to_fork, get_file_system_result_or_throw,
    to_session,
};

/// `encodeCwd` (jsonl-repo.ts:34-36) — `--` + cwd with the leading `/` or `\` stripped
/// and `/`, `\`, `:` replaced by `-`, + `--`.
fn encode_cwd(cwd: &str) -> String {
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let mut encoded = String::with_capacity(stripped.len() + 4);
    encoded.push_str("--");
    for ch in stripped.chars() {
        if ch == '/' || ch == '\\' || ch == ':' {
            encoded.push('-');
        } else {
            encoded.push(ch);
        }
    }
    encoded.push_str("--");
    encoded
}

/// Fork options — upstream `SessionForkOptions & JsonlSessionCreateOptions`
/// (jsonl-repo.ts:134-136).
#[derive(Debug, Clone, Default)]
pub struct JsonlSessionForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
    pub id: Option<String>,
    pub cwd: String,
    pub parent_session_path: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

/// `JsonlSessionRepo` (jsonl-repo.ts:38-178).
pub struct JsonlSessionRepo {
    fs: Arc<dyn FileSystem>,
    sessions_root_input: String,
    sessions_root: Mutex<Option<String>>,
}

impl JsonlSessionRepo {
    /// Constructor (jsonl-repo.ts:43-46).
    pub fn new(fs: Arc<dyn FileSystem>, sessions_root: String) -> Self {
        Self {
            fs,
            sessions_root_input: sessions_root,
            sessions_root: Mutex::new(None),
        }
    }

    /// `getSessionsRoot` (jsonl-repo.ts:48-56) — resolved and cached on first use.
    async fn get_sessions_root(&self) -> Result<String, SessionError> {
        {
            let cache = self.sessions_root.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(root) = cache.as_ref() {
                return Ok(root.clone());
            }
        }
        let root = get_file_system_result_or_throw(
            self.fs.absolute_path(&self.sessions_root_input, None).await,
            format!(
                "Failed to resolve sessions root {}",
                self.sessions_root_input
            ),
        )?;
        *self.sessions_root.lock().unwrap_or_else(|p| p.into_inner()) = Some(root.clone());
        Ok(root)
    }

    /// `getSessionDir` (jsonl-repo.ts:58-63).
    async fn get_session_dir(&self, cwd: &str) -> Result<String, SessionError> {
        get_file_system_result_or_throw(
            self.fs
                .join_path(&[self.get_sessions_root().await?, encode_cwd(cwd)], None)
                .await,
            format!("Failed to resolve session directory for {cwd}"),
        )
    }

    /// `createSessionFilePath` (jsonl-repo.ts:65-73) —
    /// `<timestamp with : and . replaced by ->_<sessionId>.jsonl`.
    async fn create_session_file_path(
        &self,
        cwd: &str,
        session_id: &str,
        timestamp: &str,
    ) -> Result<String, SessionError> {
        let file_name = format!(
            "{}_{}.jsonl",
            timestamp.replace([':', '.'], "-"),
            session_id
        );
        get_file_system_result_or_throw(
            self.fs
                .join_path(&[self.get_session_dir(cwd).await?, file_name], None)
                .await,
            format!("Failed to resolve session file path for {session_id}"),
        )
    }

    /// Full-options `fork` (jsonl-repo.ts:134-161).
    pub async fn fork(
        &self,
        source: JsonlSessionMetadata,
        options: JsonlSessionForkOptions,
    ) -> Result<Arc<dyn Session<Metadata = JsonlSessionMetadata>>, SessionError> {
        self.fork_impl(&source, &options).await
    }

    async fn fork_impl(
        &self,
        source: &JsonlSessionMetadata,
        options: &JsonlSessionForkOptions,
    ) -> Result<Arc<dyn Session<Metadata = JsonlSessionMetadata>>, SessionError> {
        let source_session = self.open(source.clone()).await?;
        let forked_entries = get_entries_to_fork(
            source_session.storage().as_ref(),
            options.entry_id.as_deref(),
            options.position,
        )
        .await?;
        let id = options.id.clone().unwrap_or_else(create_session_id);
        let created_at = create_timestamp();
        let session_dir = self.get_session_dir(&options.cwd).await?;
        get_file_system_result_or_throw(
            self.fs
                .create_dir(
                    &session_dir,
                    CreateDirOptions {
                        recursive: Some(true),
                        abort_signal: None,
                    },
                )
                .await,
            format!("Failed to create session directory {session_dir}"),
        )?;
        let file_path = self
            .create_session_file_path(&options.cwd, &id, &created_at)
            .await?;
        let storage = JsonlSessionStorage::create(
            Arc::clone(&self.fs),
            &file_path,
            JsonlSessionStorageCreateOptions {
                cwd: options.cwd.clone(),
                session_id: id,
                // `options.parentSessionPath ?? sourceMetadata.path` (jsonl-repo.ts:153).
                parent_session_path: match &options.parent_session_path {
                    Some(path) => Some(path.clone()),
                    None => Some(source.path.clone()),
                },
                // `options.metadata ?? sourceMetadata.metadata` (jsonl-repo.ts:154).
                metadata: match &options.metadata {
                    Some(metadata) => Some(metadata.clone()),
                    None => source.metadata.clone(),
                },
            },
        )
        .await?;
        for entry in forked_entries {
            storage.append_entry(entry).await?;
        }
        Ok(to_session(Arc::new(storage)))
    }

    /// `listSessionDirs` (jsonl-repo.ts:163-177) — encoded-cwd directories below the
    /// sessions root.
    async fn list_session_dirs(&self) -> Result<Vec<String>, SessionError> {
        let sessions_root = self.get_sessions_root().await?;
        let exists = get_file_system_result_or_throw(
            self.fs.exists(&sessions_root, None).await,
            format!("Failed to check sessions root {sessions_root}"),
        )?;
        if !exists {
            return Ok(Vec::new());
        }
        let entries = get_file_system_result_or_throw(
            self.fs.list_dir(&sessions_root, None).await,
            format!("Failed to list sessions root {sessions_root}"),
        )?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.kind == FileKind::Directory)
            .map(|entry| entry.path)
            .collect())
    }
}

#[async_trait]
impl SessionRepo<JsonlSessionMetadata, JsonlSessionCreateOptions, JsonlSessionListOptions>
    for JsonlSessionRepo
{
    /// `create` (jsonl-repo.ts:75-91).
    async fn create(
        &self,
        options: JsonlSessionCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = JsonlSessionMetadata>>, SessionError> {
        // `options.id ?? createSessionId()` (jsonl-repo.ts:76).
        let id = options.base.id.unwrap_or_else(create_session_id);
        let created_at = create_timestamp();
        let session_dir = self.get_session_dir(&options.cwd).await?;
        get_file_system_result_or_throw(
            self.fs
                .create_dir(
                    &session_dir,
                    CreateDirOptions {
                        recursive: Some(true),
                        abort_signal: None,
                    },
                )
                .await,
            format!("Failed to create session directory {session_dir}"),
        )?;
        let file_path = self
            .create_session_file_path(&options.cwd, &id, &created_at)
            .await?;
        let storage = JsonlSessionStorage::create(
            Arc::clone(&self.fs),
            &file_path,
            JsonlSessionStorageCreateOptions {
                cwd: options.cwd.clone(),
                session_id: id,
                parent_session_path: options.parent_session_path.clone(),
                metadata: options.metadata.clone(),
            },
        )
        .await?;
        Ok(to_session(Arc::new(storage)))
    }

    /// `open` (jsonl-repo.ts:93-101).
    async fn open(
        &self,
        metadata: JsonlSessionMetadata,
    ) -> Result<Arc<dyn Session<Metadata = JsonlSessionMetadata>>, SessionError> {
        let exists = get_file_system_result_or_throw(
            self.fs.exists(&metadata.path, None).await,
            format!("Failed to check session {}", metadata.path),
        )?;
        if !exists {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Session not found: {}", metadata.path),
            ));
        }
        let storage = JsonlSessionStorage::open(Arc::clone(&self.fs), &metadata.path).await?;
        Ok(to_session(Arc::new(storage)))
    }

    /// `list` (jsonl-repo.ts:103-125) — per-cwd directory or all encoded-cwd
    /// directories; corrupt session files are skipped.
    async fn list(
        &self,
        options: JsonlSessionListOptions,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        let dirs = match &options.cwd {
            Some(cwd) => vec![self.get_session_dir(cwd).await?],
            None => self.list_session_dirs().await?,
        };
        let mut sessions = Vec::new();
        for dir in dirs {
            let exists = get_file_system_result_or_throw(
                self.fs.exists(&dir, None).await,
                format!("Failed to check session directory {dir}"),
            )?;
            if !exists {
                continue;
            }
            let files = get_file_system_result_or_throw(
                self.fs.list_dir(&dir, None).await,
                format!("Failed to list sessions in {dir}"),
            )?;
            for file in files {
                if file.kind == FileKind::Directory || !file.name.ends_with(".jsonl") {
                    continue;
                }
                match load_jsonl_session_metadata(self.fs.as_ref(), &file.path).await {
                    Ok(metadata) => sessions.push(metadata),
                    // Corrupt session files are skipped, like upstream
                    // (jsonl-repo.ts:115-120); other errors propagate.
                    Err(error) if error.code == SessionErrorCode::InvalidSession => {}
                    Err(error) => return Err(error),
                }
            }
        }
        // `new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime()`
        // (jsonl-repo.ts:123) — newest first.
        sessions.sort_by(|a, b| {
            let a_ms = parse_iso8601_ms(&a.base.created_at).unwrap_or(0);
            let b_ms = parse_iso8601_ms(&b.base.created_at).unwrap_or(0);
            b_ms.cmp(&a_ms)
        });
        Ok(sessions)
    }

    /// `delete` (jsonl-repo.ts:127-132).
    async fn delete(&self, metadata: JsonlSessionMetadata) -> Result<(), SessionError> {
        get_file_system_result_or_throw(
            self.fs
                .remove(
                    &metadata.path,
                    RemoveOptions {
                        recursive: false,
                        force: true,
                        abort_signal: None,
                    },
                )
                .await,
            format!("Failed to delete session {}", metadata.path),
        )?;
        Ok(())
    }

    /// Trait surface `fork`: no `entryId` / `position` (types.rs:1023-1029) — full
    /// copy. See the inherent [`JsonlSessionRepo::fork`] for the upstream options.
    async fn fork(
        &self,
        source: JsonlSessionMetadata,
        options: JsonlSessionCreateOptions,
    ) -> Result<Arc<dyn Session<Metadata = JsonlSessionMetadata>>, SessionError> {
        self.fork_impl(
            &source,
            &JsonlSessionForkOptions {
                entry_id: None,
                position: None,
                id: options.base.id,
                cwd: options.cwd,
                parent_session_path: options.parent_session_path,
                metadata: options.metadata,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    /// `expect_err` without a `T: Debug` bound (the storage types are not `Debug`).
    fn expect_err<T, E>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: expected an error"),
            Err(error) => error,
        }
    }
    use crate::harness::session::repo_utils::test_support::{
        TestFs, assistant_message, user_message,
    };
    use crate::harness::types::{Session, SessionCreateOptions, SessionEntryCursorOptions};

    use super::*;

    fn repo(fs: Arc<TestFs>) -> JsonlSessionRepo {
        let root = fs.root().to_string_lossy().into_owned();
        JsonlSessionRepo::new(fs, root)
    }

    fn create_options(id: &str, cwd: &str) -> JsonlSessionCreateOptions {
        JsonlSessionCreateOptions {
            base: SessionCreateOptions {
                id: Some(id.to_owned()),
            },
            cwd: cwd.to_owned(),
            parent_session_path: None,
            metadata: None,
        }
    }

    async fn entry_ids(session: &dyn Session<Metadata = JsonlSessionMetadata>) -> Vec<String> {
        session
            .get_entries(SessionEntryCursorOptions::default())
            .await
            .expect("entries")
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn test_create_stores_below_encoded_cwd_dirs_and_lists_by_cwd() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        let session = repo
            .create(create_options(
                "019de8c2-de29-73e9-ae0c-e134db34c447",
                "/tmp/my-project",
            ))
            .await
            .expect("create");
        let other = repo
            .create(create_options("other-session", "/tmp/other-project"))
            .await
            .expect("create");
        let metadata = session.get_metadata().await.expect("metadata");
        let other_metadata = other.get_metadata().await.expect("metadata");
        assert!(
            metadata.path.contains("--tmp-my-project--"),
            "{}",
            metadata.path
        );
        assert!(
            other_metadata.path.contains("--tmp-other-project--"),
            "{}",
            other_metadata.path
        );
        assert!(std::path::Path::new(&metadata.path).exists());
        let by_cwd = repo
            .list(JsonlSessionListOptions {
                cwd: Some("/tmp/my-project".to_owned()),
            })
            .await
            .expect("list");
        let by_cwd_ids: Vec<String> = by_cwd.iter().map(|m| m.base.id.clone()).collect();
        assert_eq!(by_cwd_ids, [metadata.base.id.as_str()]);
        let all = repo
            .list(JsonlSessionListOptions::default())
            .await
            .expect("list");
        let mut all_ids: Vec<String> = all.iter().map(|m| m.base.id.clone()).collect();
        all_ids.sort();
        assert_eq!(
            all_ids,
            ["019de8c2-de29-73e9-ae0c-e134db34c447", "other-session"]
        );
    }

    #[tokio::test]
    async fn test_open_delete_and_fork_by_metadata() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        // Build the source session through the repo's `create` + the `Session`
        // facade (append methods land with the session.rs port; repo-utils.ts
        // `toSession` wraps the storage in the facade).
        let source = repo
            .create(create_options("source-session", "/tmp/source"))
            .await
            .expect("create");
        let user1 = source
            .append_message(user_message("one"))
            .await
            .expect("append user");
        let assistant1 = source
            .append_message(assistant_message("two"))
            .await
            .expect("append assistant");
        let user2 = source
            .append_message(user_message("three"))
            .await
            .expect("append user");
        let source_metadata = source.get_metadata().await.expect("metadata");

        let opened = repo.open(source_metadata.clone()).await.expect("open");
        assert_eq!(
            opened.get_metadata().await.expect("metadata"),
            source_metadata
        );

        // Fork before `user2` (a user message): copies up to its parent.
        let fork = repo
            .fork(
                source_metadata.clone(),
                JsonlSessionForkOptions {
                    entry_id: Some(user2.clone()),
                    position: None,
                    id: Some("fork-session".to_owned()),
                    cwd: "/tmp/target".to_owned(),
                    parent_session_path: None,
                    metadata: None,
                },
            )
            .await
            .expect("fork");
        let fork_metadata = fork.get_metadata().await.expect("metadata");
        assert_eq!(fork_metadata.cwd, "/tmp/target");
        assert_eq!(
            fork_metadata.parent_session_path.as_deref(),
            Some(source_metadata.path.as_str())
        );
        assert!(fork_metadata.path.ends_with("_fork-session.jsonl"));
        assert_eq!(
            entry_ids(fork.as_ref()).await,
            [user1.as_str(), assistant1.as_str()]
        );

        let full_fork = repo
            .fork(
                source_metadata.clone(),
                JsonlSessionForkOptions {
                    id: Some("full-fork-session".to_owned()),
                    cwd: "/tmp/target".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("fork");
        assert_eq!(
            entry_ids(full_fork.as_ref()).await,
            [user1.as_str(), assistant1.as_str(), user2.as_str()]
        );

        repo.delete(source_metadata.clone()).await.expect("delete");
        assert!(!std::path::Path::new(&source_metadata.path).exists());
        let error = expect_err(repo.open(source_metadata).await, "open after delete");
        assert_eq!(error.code, SessionErrorCode::NotFound);
    }

    #[tokio::test]
    async fn test_header_metadata_preserved_through_create_list_and_fork() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        let mut profile = Map::new();
        profile.insert("profile".to_owned(), json!("reviewer"));
        let source = repo
            .create(JsonlSessionCreateOptions {
                base: SessionCreateOptions {
                    id: Some("source-session".to_owned()),
                },
                cwd: "/tmp/source".to_owned(),
                parent_session_path: None,
                metadata: Some(profile.clone()),
            })
            .await
            .expect("create");
        let source_metadata = source.get_metadata().await.expect("metadata");
        assert_eq!(source_metadata.metadata, Some(profile.clone()));
        let listed = repo
            .list(JsonlSessionListOptions {
                cwd: Some("/tmp/source".to_owned()),
            })
            .await
            .expect("list");
        let listed_metadata: Vec<Option<Map<String, Value>>> =
            listed.iter().map(|m| m.metadata.clone()).collect();
        assert_eq!(listed_metadata, [Some(profile.clone())]);
        // Fork inherits the source header metadata (`options.metadata ?? source.metadata`).
        let fork = repo
            .fork(
                source_metadata.clone(),
                JsonlSessionForkOptions {
                    id: Some("fork-session".to_owned()),
                    cwd: "/tmp/target".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("fork");
        assert_eq!(
            fork.get_metadata().await.expect("metadata").metadata,
            Some(profile.clone())
        );
        // An explicit metadata override wins.
        let mut writer = Map::new();
        writer.insert("profile".to_owned(), json!("writer"));
        let overridden = repo
            .fork(
                source_metadata,
                JsonlSessionForkOptions {
                    id: Some("overridden-session".to_owned()),
                    cwd: "/tmp/target".to_owned(),
                    metadata: Some(writer.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("fork");
        assert_eq!(
            overridden.get_metadata().await.expect("metadata").metadata,
            Some(writer)
        );
    }

    #[tokio::test]
    async fn test_list_skips_corrupt_session_files() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        repo.create(create_options("good-session", "/tmp/project"))
            .await
            .expect("create");
        // A corrupt .jsonl in the same encoded-cwd dir is skipped (jsonl-repo.ts:115-120).
        let dir = fs.root().join("--tmp-project--");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("corrupt.jsonl"), "not json\n").expect("write");
        let listed = repo
            .list(JsonlSessionListOptions {
                cwd: Some("/tmp/project".to_owned()),
            })
            .await
            .expect("list");
        let ids: Vec<String> = listed.iter().map(|m| m.base.id.clone()).collect();
        assert_eq!(ids, ["good-session"]);
    }

    #[tokio::test]
    async fn test_list_sorts_by_created_at_across_timestamp_variants() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        // Session files written directly (the repo's `create` always writes
        // `new Date().toISOString()` — `...SS.sssZ`): `list` sorts by
        // `createdAt` via `Date.parse` semantics (jsonl-repo.ts:123), which
        // also accepts millisecond-free and timezone-offset ISO 8601
        // (parse_iso8601_ms).
        let dir = fs.root().join("--tmp-project--");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let sessions = [
            ("millis-z", "2026-01-02T03:04:05.000Z"),
            ("no-millis", "2026-01-02T03:04:06Z"),
            ("offset", "2026-01-01T23:04:07-04:00"),
        ];
        for (id, timestamp) in sessions {
            let header = json!({
                "type": "session",
                "version": 3,
                "id": id,
                "timestamp": timestamp,
                "cwd": "/tmp/project",
            });
            std::fs::write(
                dir.join(format!("{id}.jsonl")),
                format!("{}\n", serde_json::to_string(&header).expect("json")),
            )
            .expect("write session file");
        }
        let listed = repo
            .list(JsonlSessionListOptions {
                cwd: Some("/tmp/project".to_owned()),
            })
            .await
            .expect("list");
        let ids: Vec<String> = listed.iter().map(|m| m.base.id.clone()).collect();
        // Newest first: `2026-01-01T23:04:07-04:00` == `2026-01-02T03:04:07Z`.
        assert_eq!(ids, ["offset", "no-millis", "millis-z"]);
    }

    #[tokio::test]
    async fn test_fork_trait_surface_copies_full_session() {
        let fs = TestFs::new();
        let repo = repo(fs.clone());
        let source = repo
            .create(create_options("source-session", "/tmp/source"))
            .await
            .expect("create");
        let source_metadata = source.get_metadata().await.expect("metadata");
        let fork = SessionRepo::fork(
            &repo,
            source_metadata.clone(),
            JsonlSessionCreateOptions {
                base: SessionCreateOptions {
                    id: Some("trait-fork".to_owned()),
                },
                cwd: "/tmp/target".to_owned(),
                parent_session_path: None,
                metadata: None,
            },
        )
        .await
        .expect("fork");
        let fork_metadata = fork.get_metadata().await.expect("metadata");
        assert_eq!(fork_metadata.base.id, "trait-fork");
        assert_eq!(
            fork_metadata.parent_session_path.as_deref(),
            Some(source_metadata.path.as_str())
        );
    }
}
