//! Document definitions and typed access rules (port of `packages/durable/src/documents.ts`
//! @ `abe508e1b`).
//!
//! A definition describes one logical document address: its kind, version,
//! scope, conversation history/fork semantics, initializer, migration, and
//! checkpoint predicate. Tokens ([`DocToken`]) carry a definition; the session
//! caches materialized incarnations by [`address_id`].
//!
//! Intentional differences: upstream overloads resolve the owner/key argument
//! list at the call site; the Rust port passes a [`DocTarget`] explicitly and
//! validates it against the definition scope. Initializers and migrations are
//! closure values instead of object methods.

use std::sync::Arc;

use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::chord::delta::Op;
use crate::chord::json::JsonObject;
use crate::ids::{ConversationId, DocumentId, TaskId};
use crate::types::{
    DocumentAddress, DocumentCreate, DocumentFork, DocumentHistory, DocumentRecord, DocumentScope,
    StoredDocument,
};

/// Definition scope before an owner is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocScope {
    /// Singleton document shared by the whole session.
    Session,
    /// Conversation-scoped singleton or family.
    Conversation,
    /// Task-scoped singleton or family.
    Task,
}

/// Initializer callback (upstream `initial`): produces the initial root value.
pub type DocInitializer = Arc<dyn Fn(Option<&JsonValue>) -> JsonObject + Send + Sync>;
/// Migration callback (upstream `migrate`): upgrades a value from an older version.
pub type DocMigration = Arc<dyn Fn(&JsonValue, u64) -> JsonValue + Send + Sync>;
/// Checkpoint predicate (upstream `checkpointWhen`).
pub type DocCheckpoint = Arc<dyn Fn(&JsonValue, &[Op], u64) -> bool + Send + Sync>;

/// One document definition (upstream `DocDefinition`/`DocFamilyDefinition`).
pub struct DocDefinition {
    /// Stable document kind, such as `pi.agent`.
    pub kind: String,
    /// Definition version; stored versions may migrate up to it.
    pub version: u64,
    /// Logical scope.
    pub scope: DocScope,
    /// Conversation history retention; required for conversation scope.
    pub history: Option<DocumentHistory>,
    /// Conversation fork initialization; required for conversation scope.
    pub fork: Option<DocumentFork>,
    /// Whether callers must supply a family key.
    pub family: bool,
    /// Initial value; family definitions receive the caller's seed.
    pub initial: DocInitializer,
    /// Migrate a stored value from an older definition version.
    pub migrate: Option<DocMigration>,
    /// Whether a nonempty delta batch must be stored as a fresh base.
    pub checkpoint_when: Option<DocCheckpoint>,
}

impl std::fmt::Debug for DocDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocDefinition")
            .field("kind", &self.kind)
            .field("version", &self.version)
            .field("scope", &self.scope)
            .field("history", &self.history)
            .field("fork", &self.fork)
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

/// Typed singleton or family token (upstream `DocToken`/`DocFamilyToken`).
#[derive(Clone)]
pub struct DocToken {
    /// The carried definition.
    pub definition: Arc<DocDefinition>,
}

impl std::fmt::Debug for DocToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocToken")
            .field("definition", &self.definition)
            .finish()
    }
}

/// Validate a definition and wrap it in a singleton token.
pub fn define_doc(definition: DocDefinition) -> DocToken {
    validate_definition(&definition);
    DocToken {
        definition: Arc::new(definition),
    }
}

/// Validate a definition and wrap it in a family token.
pub fn define_doc_family(mut definition: DocDefinition) -> DocToken {
    definition.family = true;
    validate_definition(&definition);
    DocToken {
        definition: Arc::new(definition),
    }
}

fn validate_definition(definition: &DocDefinition) {
    assert!(
        definition.version >= 1,
        "Document {} version must be a positive integer",
        definition.kind
    );
    if definition.scope == DocScope::Conversation {
        assert!(
            definition.history.is_some() && definition.fork.is_some(),
            "Document {} conversation scope requires history and fork",
            definition.kind
        );
    }
}

/// Failure modes of typed document access (upstream `TypeError`/`Error`).
#[derive(Debug, Clone, Error)]
pub enum DocumentError {
    /// The supplied target does not match the definition scope or family shape.
    #[error("{0}")]
    Target(String),
    /// The persisted incarnation disagrees with the definition semantics.
    #[error("Document {id} ({kind}) does not match the supplied definition semantics")]
    Scope {
        /// Incarnation ID.
        id: u64,
        /// Persisted kind.
        kind: String,
    },
    /// The stored version is newer than the definition.
    #[error("Document {id} ({kind}) has newer version {stored} than {defined}")]
    NewerVersion {
        /// Incarnation ID.
        id: u64,
        /// Persisted kind.
        kind: String,
        /// Stored version.
        stored: u64,
        /// Definition version.
        defined: u64,
    },
    /// The stored version is older and the definition has no migration.
    #[error("Document {id} ({kind}) requires migration from version {stored}")]
    MigrationRequired {
        /// Incarnation ID.
        id: u64,
        /// Persisted kind.
        kind: String,
        /// Stored version.
        stored: u64,
    },
}

/// Owner argument of document resolution (upstream overload arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocTarget<'a> {
    /// Session-scoped singleton.
    Session,
    /// Session-scoped family member (V17-14 addition: upstream resolves a
    /// session family key like any other family).
    SessionFamily(&'a str),
    /// Conversation-scoped singleton or family member.
    Conversation {
        /// Owning conversation.
        conversation_id: ConversationId,
        /// Family key.
        key: Option<&'a str>,
    },
    /// Task-scoped singleton or family member.
    Task {
        /// Owning task.
        task_id: TaskId,
        /// Family key.
        key: Option<&'a str>,
    },
}

/// Logical address plus its string identity for caches (upstream `ResolvedAddress`).
#[derive(Debug, Clone)]
pub struct ResolvedAddress {
    /// Logical address.
    pub address: DocumentAddress,
    /// Stable string identity.
    pub id: String,
}

/// Resolve a definition and target into an address.
pub fn resolve_address(
    definition: &DocDefinition,
    target: DocTarget<'_>,
) -> Result<ResolvedAddress, DocumentError> {
    let address = match (definition.scope, target) {
        (DocScope::Session, DocTarget::Session) => DocumentAddress {
            kind: definition.kind.clone(),
            scope: DocumentScope::Session,
            key: None,
        },
        (DocScope::Session, DocTarget::SessionFamily(key)) => DocumentAddress {
            kind: definition.kind.clone(),
            scope: DocumentScope::Session,
            key: validate_key(definition, Some(key))?,
        },
        (
            DocScope::Conversation,
            DocTarget::Conversation {
                conversation_id,
                key,
            },
        ) => DocumentAddress {
            kind: definition.kind.clone(),
            scope: DocumentScope::Conversation { conversation_id },
            key: validate_key(definition, key)?,
        },
        (DocScope::Task, DocTarget::Task { task_id, key }) => DocumentAddress {
            kind: definition.kind.clone(),
            scope: DocumentScope::Task { task_id },
            key: validate_key(definition, key)?,
        },
        _ => {
            return Err(DocumentError::Target(format!(
                "Document {} requires a {} target",
                definition.kind,
                match definition.scope {
                    DocScope::Session => "session",
                    DocScope::Conversation => "conversation",
                    DocScope::Task => "task",
                }
            )));
        }
    };
    let id = address_id(&address);
    Ok(ResolvedAddress { address, id })
}

fn validate_key(
    definition: &DocDefinition,
    key: Option<&str>,
) -> Result<Option<String>, DocumentError> {
    match (definition.family, key) {
        (true, Some(key)) => Ok(Some(key.to_string())),
        (true, None) => Err(DocumentError::Target(format!(
            "Document {} requires a family key",
            definition.kind
        ))),
        (false, Some(_)) => Err(DocumentError::Target(format!(
            "Document {} is a singleton and takes no key",
            definition.kind
        ))),
        (false, None) => Ok(None),
    }
}

/// Stable string identity of one logical address: the exact
/// `JSON.stringify([kind, scopeKind, owner, key ?? null])` shape
/// (`documents.ts:150`).
pub fn address_id(address: &DocumentAddress) -> String {
    let (scope_kind, owner): (&str, Option<u64>) = match &address.scope {
        DocumentScope::Session => ("session", None),
        DocumentScope::Conversation { conversation_id } => {
            ("conversation", Some(conversation_id.get()))
        }
        DocumentScope::Task { task_id } => ("task", Some(task_id.get())),
    };
    serde_json::to_string(&serde_json::json!([
        address.kind,
        scope_kind,
        owner,
        address.key
    ]))
    .unwrap_or_default()
}

/// Build the storage create record for a new incarnation at an address
/// (`documents.ts:160`).
pub fn document_create(
    definition: &DocDefinition,
    address: &DocumentAddress,
    id: DocumentId,
) -> DocumentCreate {
    match address.scope {
        DocumentScope::Session | DocumentScope::Task { .. } => DocumentCreate {
            id,
            kind: definition.kind.clone(),
            key: address.key.clone(),
            scope: address.scope.clone(),
            history: None,
            fork: None,
        },
        DocumentScope::Conversation { .. } => DocumentCreate {
            id,
            kind: definition.kind.clone(),
            key: address.key.clone(),
            scope: address.scope.clone(),
            history: definition.history,
            fork: definition.fork,
        },
    }
}

/// Reject typed access whose token disagrees with the persisted scope/history/fork
/// (`documents.ts:179`).
pub fn check_record_scope(
    definition: &DocDefinition,
    record: &DocumentCreate,
) -> Result<(), DocumentError> {
    let matches = match (&record.scope, definition.scope) {
        (DocumentScope::Session, DocScope::Session) => true,
        (DocumentScope::Task { .. }, DocScope::Task) => true,
        (DocumentScope::Conversation { .. }, DocScope::Conversation) => {
            record.history == definition.history && record.fork == definition.fork
        }
        _ => false,
    };
    if !matches {
        return Err(DocumentError::Scope {
            id: record.id.get(),
            kind: record.kind.clone(),
        });
    }
    Ok(())
}

/// Same scope check against a persisted record.
pub fn check_record_scope_record(
    definition: &DocDefinition,
    record: &DocumentRecord,
) -> Result<(), DocumentError> {
    check_record_scope(definition, &create_from_record(record))
}

/// Same version check against a persisted record (V17-14 addition).
pub fn check_record_version_record(
    definition: &DocDefinition,
    record: &DocumentRecord,
    version: u64,
) -> Result<(), DocumentError> {
    check_record_version(definition, &create_from_record(record), version)
}

fn create_from_record(record: &DocumentRecord) -> DocumentCreate {
    DocumentCreate {
        id: record.id,
        kind: record.kind.clone(),
        key: record.key.clone(),
        scope: record.scope.clone(),
        history: record.history,
        fork: record.fork,
    }
}

/// Reject typed access to a stored version the definition cannot use
/// (`documents.ts:190`).
pub fn check_record_version(
    definition: &DocDefinition,
    record: &DocumentCreate,
    version: u64,
) -> Result<(), DocumentError> {
    if version > definition.version {
        return Err(DocumentError::NewerVersion {
            id: record.id.get(),
            kind: record.kind.clone(),
            stored: version,
            defined: definition.version,
        });
    }
    if version < definition.version && definition.migrate.is_none() {
        return Err(DocumentError::MigrationRequired {
            id: record.id.get(),
            kind: record.kind.clone(),
            stored: version,
        });
    }
    Ok(())
}

/// Validate and materialize a detached stored value (`documents.ts:199`).
pub fn materialize_document(
    definition: &DocDefinition,
    stored: &StoredDocument,
) -> Result<JsonObject, DocumentError> {
    materialize_document_value(definition, &stored.record, stored.version, &stored.value)
}

/// Validate and materialize one detached value (`documents.ts:204`).
pub fn materialize_document_value(
    definition: &DocDefinition,
    record: &DocumentRecord,
    version: u64,
    value: &JsonObject,
) -> Result<JsonObject, DocumentError> {
    check_record_scope_record(definition, record)?;
    check_record_version(
        definition,
        &DocumentCreate {
            id: record.id,
            kind: record.kind.clone(),
            key: record.key.clone(),
            scope: record.scope.clone(),
            history: record.history,
            fork: record.fork,
        },
        version,
    )?;
    if version == definition.version {
        return Ok(value.clone());
    }
    let migrate = definition.migrate.as_ref().expect("migration checked");
    match migrate(&JsonValue::Object(value.clone()), version) {
        JsonValue::Object(migrated) => Ok(migrated),
        _ => Ok(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{Conversation, Document, Task, id_from_number};
    use crate::types::{CurrentMarker, DocumentPoint};

    fn definition(scope: DocScope, family: bool) -> DocDefinition {
        DocDefinition {
            kind: "pi.test".to_string(),
            version: 1,
            scope,
            history: (scope == DocScope::Conversation).then_some(DocumentHistory::Latest),
            fork: (scope == DocScope::Conversation).then_some(DocumentFork::Initial),
            family,
            initial: Arc::new(|seed| match seed {
                Some(JsonValue::Object(map)) => map.clone(),
                _ => JsonObject::new(),
            }),
            migrate: None,
            checkpoint_when: None,
        }
    }

    #[test]
    fn address_id_matches_upstream_json_shape() {
        let address = DocumentAddress {
            kind: "pi.agent".to_string(),
            scope: DocumentScope::Conversation {
                conversation_id: id_from_number::<Conversation>(1),
            },
            key: None,
        };
        assert_eq!(
            address_id(&address),
            "[\"pi.agent\",\"conversation\",1,null]"
        );
        let family = DocumentAddress {
            kind: "pi.doc".to_string(),
            scope: DocumentScope::Task {
                task_id: id_from_number::<Task>(7),
            },
            key: Some("k".to_string()),
        };
        assert_eq!(address_id(&family), "[\"pi.doc\",\"task\",7,\"k\"]");
        assert_eq!(
            address_id(&DocumentAddress {
                kind: "pi.session".to_string(),
                scope: DocumentScope::Session,
                key: None,
            }),
            "[\"pi.session\",\"session\",null,null]"
        );
    }

    #[test]
    fn resolve_address_checks_scope_and_key() {
        let def = definition(DocScope::Conversation, false);
        let resolved = resolve_address(
            &def,
            DocTarget::Conversation {
                conversation_id: id_from_number::<Conversation>(2),
                key: None,
            },
        )
        .unwrap();
        assert_eq!(resolved.id, "[\"pi.test\",\"conversation\",2,null]");
        assert!(
            resolve_address(&def, DocTarget::Session).is_err(),
            "scope mismatch"
        );
        let family = definition(DocScope::Conversation, true);
        assert!(
            resolve_address(
                &family,
                DocTarget::Conversation {
                    conversation_id: id_from_number::<Conversation>(2),
                    key: None,
                }
            )
            .is_err(),
            "family requires a key"
        );
    }

    #[test]
    fn document_create_carries_conversation_semantics() {
        let def = definition(DocScope::Conversation, false);
        let address = DocumentAddress {
            kind: def.kind.clone(),
            scope: DocumentScope::Conversation {
                conversation_id: id_from_number::<Conversation>(3),
            },
            key: None,
        };
        let record = document_create(&def, &address, id_from_number::<Document>(5));
        assert_eq!(record.history, Some(DocumentHistory::Latest));
        assert_eq!(record.fork, Some(DocumentFork::Initial));
        check_record_scope(&def, &record).unwrap();
    }

    #[test]
    fn version_checks_require_migration() {
        let mut def = definition(DocScope::Session, false);
        let record = DocumentCreate {
            id: id_from_number::<Document>(1),
            kind: def.kind.clone(),
            key: None,
            scope: DocumentScope::Session,
            history: None,
            fork: None,
        };
        assert!(check_record_version(&def, &record, 0).is_err());
        def.migrate = Some(Arc::new(|value, _version| {
            let mut map = value.as_object().cloned().unwrap_or_default();
            map.insert("migrated".to_string(), JsonValue::Bool(true));
            JsonValue::Object(map)
        }));
        check_record_version(&def, &record, 0).unwrap();
        let stored = StoredDocument {
            record: DocumentRecord {
                id: record.id,
                kind: record.kind.clone(),
                key: None,
                created_at: crate::ids::seq_from_number(1),
                retired_at: None,
                scope: DocumentScope::Session,
                history: None,
                fork: None,
            },
            version: 0,
            value: JsonObject::from_iter([("k".to_string(), JsonValue::from(1))]),
            deltas_since_base: 0,
        };
        let materialized = materialize_document(&def, &stored).unwrap();
        assert_eq!(materialized.get("migrated"), Some(&JsonValue::Bool(true)));
        let _ = DocumentPoint::Current(CurrentMarker::Current);
    }
}
