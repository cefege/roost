//! Idempotent import of native host commands and pre-Durable model history.
//! Roost retains product records and request admission; the kernel owns model
//! context, immutable edits and generated work. Projected replies never reimport.

mod legacy;

use legacy::enrich_legacy_message;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::{Arc, OnceLock, Weak};

use rpi_ai::types::Message;
use rpi_durable::documents::{DocDefinition, DocScope};
use rpi_durable::harness::HarnessImpl;
use rpi_durable::session::{DocOwner, EntryDraft, SessionResult, commit_fn};
use rpi_durable::types::{ContextEdit, ContextEditAction, DocumentFork, DocumentHistory};
use rpi_durable::{Context, DocToken, EntryId, ROOT_CONVERSATION_ID, define_doc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::state::ProjectionState;
use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::AgentRuntime;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct ImportedState {
    sources: BTreeMap<u64, Source>,
    rewind_sequence: u64,
}

#[derive(Serialize, Deserialize)]
struct Source {
    entry: EntryId,
    fingerprint: String,
    active: bool,
}

pub(super) struct JournalSync {
    runtime: AgentRuntime,
    conversation_id: String,
    token: DocToken,
    projection: Arc<ProjectionState>,
    harness: OnceLock<Weak<HarnessImpl>>,
    line: tokio::sync::Mutex<()>,
}

impl JournalSync {
    pub fn new(
        runtime: AgentRuntime,
        conversation_id: String,
        projection: Arc<ProjectionState>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            conversation_id,
            projection,
            token: define_doc(DocDefinition {
                kind: "roost.host-context".into(),
                version: 1,
                scope: DocScope::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
                family: false,
                initial: Arc::new(|_| serde_json::Map::new()),
                migrate: None,
                checkpoint_when: Some(Arc::new(|_, _, _| true)),
            }),
            harness: OnceLock::new(),
            line: tokio::sync::Mutex::new(()),
        })
    }

    pub fn bind(&self, harness: &Arc<HarnessImpl>) -> Result<(), AgentError> {
        self.harness
            .set(Arc::downgrade(harness))
            .map_err(|_| AgentError::Store("Durable harness is already bound".into()))?;
        Ok(())
    }

    pub async fn current_agent(
        &self,
        id: rpi_durable::ConversationId,
        context: &Context,
    ) -> SessionResult<rpi_durable::harness::Agent> {
        let harness = self.harness.get().and_then(Weak::upgrade).ok_or_else(|| {
            rpi_durable::session::SessionError::message("Durable harness is closed")
        })?;
        harness.resolve_agent(id, None, context).await
    }

    pub async fn synchronize(
        &self,
        entries: &[(u64, Entry)],
        provider: &str,
        context: &Context,
    ) -> Result<(), AgentError> {
        let _guard = self.line.lock().await;
        let harness = self
            .harness
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::Store("Durable harness is closed".into()))?;
        let mut groups: Vec<(u64, Vec<Message>)> = Vec::new();
        let mut indices = HashMap::new();
        let source_entries: HashMap<_, _> = entries
            .iter()
            .map(|(sequence, entry)| (*sequence, entry))
            .collect();
        let rewind_sequence = entries
            .iter()
            .rev()
            .find_map(|(sequence, entry)| {
                matches!(entry, Entry::Rewind { .. }).then_some(*sequence)
            })
            .unwrap_or(0);
        let retained_summary = crate::journal::effective_entries(entries)
            .iter()
            .rev()
            .find_map(|(sequence, entry)| {
                matches!(entry, Entry::Compaction { .. }).then_some(*sequence)
            });
        for (sequence, message) in crate::history::build_message_entries(entries, provider) {
            let index = *indices.entry(sequence).or_insert_with(|| {
                groups.push((sequence, Vec::new()));
                groups.len() - 1
            });
            let mut message = super::message::to_durable_message(&message)?;
            if let Some(source) = source_entries.get(&sequence) {
                enrich_legacy_message(&mut message, source);
            }
            groups[index].1.push(message);
        }
        let projected = self
            .projection
            .projected
            .lock()
            .map_err(|_| AgentError::Store("Durable provenance lock poisoned".into()))?
            .clone();
        let token = self.token.clone();
        let mappings = harness
            .session()
            .commit(
                commit_fn(move |tx| {
                    let groups = groups.clone();
                    let token = token.clone();
                    let projected = projected.clone();
                    Box::pin(async move {
                        let draft = tx
                            .doc(&token, DocOwner::Conversation(ROOT_CONVERSATION_ID), None)
                            .await?;
                        let value = draft.read()?;
                        let mut state: ImportedState =
                            serde_json::from_value(value).map_err(|error| {
                                rpi_durable::session::SessionError::message(error.to_string())
                            })?;
                        for (sequence, entry) in &projected {
                            state.sources.entry(*sequence).or_insert_with(|| Source {
                                entry: *entry,
                                fingerprint: String::new(),
                                active: true,
                            });
                        }
                        let desired: HashSet<u64> =
                            groups.iter().map(|(sequence, _)| *sequence).collect();
                        let restore_head = rewind_sequence > state.rewind_sequence;
                        let retained_summary_model = if restore_head {
                            if let Some(sequence) = retained_summary {
                                if let Some(source) = state.sources.get(&sequence) {
                                    tx.entry(source.entry).await?.and_then(|entry| entry.model)
                                } else {
                                    groups
                                        .iter()
                                        .find(|(source, _)| *source == sequence)
                                        .map(|(_, messages)| messages.clone())
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let mut edits = Vec::new();
                        let mut restored = HashMap::new();
                        for (sequence, source) in &state.sources {
                            if !source.active
                                && desired.contains(sequence)
                                && projected.contains_key(sequence)
                                && let Some(entry) = tx.entry(source.entry).await?
                            {
                                restored.insert(*sequence, entry.model.unwrap_or_default());
                            }
                        }
                        for (sequence, source) in &mut state.sources {
                            if source.active && !desired.contains(sequence) {
                                edits.push(ContextEdit {
                                    target: source.entry,
                                    action: ContextEditAction::Omit,
                                });
                                source.active = false;
                            }
                        }
                        for (sequence, messages) in groups {
                            if projected.contains_key(&sequence) {
                                if let Some(source) = state.sources.get_mut(&sequence)
                                    && !source.active
                                {
                                    edits.push(ContextEdit {
                                        target: source.entry,
                                        action: ContextEditAction::Replace {
                                            messages: restored
                                                .remove(&sequence)
                                                .unwrap_or(messages),
                                        },
                                    });
                                    source.active = true;
                                }
                                continue;
                            }
                            let fingerprint = fingerprint(&messages)?;
                            if let Some(source) = state.sources.get_mut(&sequence) {
                                if !source.active || source.fingerprint != fingerprint {
                                    edits.push(ContextEdit {
                                        target: source.entry,
                                        action: ContextEditAction::Replace { messages },
                                    });
                                    source.fingerprint = fingerprint;
                                    source.active = true;
                                }
                            } else {
                                let mut entry = EntryDraft::new("roost.host-entry");
                                entry.model = Some(messages);
                                entry.data = Some(serde_json::json!({"journalSequence": sequence}));
                                let appended = tx.append_entry(ROOT_CONVERSATION_ID, entry).await?;
                                state.sources.insert(
                                    sequence,
                                    Source {
                                        entry: appended.id,
                                        fingerprint,
                                        active: true,
                                    },
                                );
                            }
                        }
                        if !edits.is_empty() || restore_head {
                            if restore_head
                                && let Some(source) = retained_summary
                                    .and_then(|sequence| state.sources.get(&sequence))
                            {
                                edits.push(ContextEdit {
                                    target: source.entry,
                                    action: ContextEditAction::Omit,
                                });
                            }
                            let mut edit = EntryDraft::new("roost.host-context-edit");
                            edit.edits = Some(edits);
                            if restore_head {
                                // A replacement head excludes older heads, including a surviving summary.
                                edit.model = retained_summary_model;
                                edit.head = Some(
                                    state
                                        .sources
                                        .values()
                                        .filter(|source| source.active)
                                        .map(|source| source.entry)
                                        .min()
                                        .map(rpi_durable::session::EntryHead::At)
                                        .unwrap_or(rpi_durable::session::EntryHead::This),
                                );
                                state.rewind_sequence = rewind_sequence;
                            }
                            tx.append_entry(ROOT_CONVERSATION_ID, edit).await?;
                        }
                        let mappings: Vec<_> = state
                            .sources
                            .iter()
                            .map(|(sequence, source)| (source.entry, *sequence))
                            .collect();
                        let value = serde_json::to_value(state).map_err(|error| {
                            rpi_durable::session::SessionError::message(error.to_string())
                        })?;
                        *draft.write()? = value;
                        Ok(mappings)
                    })
                }),
                context,
            )
            .await
            .map_err(session_error)?;
        let mut origins = self
            .projection
            .origins
            .lock()
            .map_err(|_| AgentError::Store("Durable origin lock poisoned".into()))?;
        origins.extend(mappings);
        Ok(())
    }

    pub async fn dispatch_boundary(&self, context: &Context) -> Result<Vec<Message>, AgentError> {
        let harness = self
            .harness
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::Store("Durable harness is closed".into()))?;
        let cancel = context
            .abort_signal()
            .map(|signal| signal.token())
            .unwrap_or_default();
        let record = self.runtime.record(&self.conversation_id).await?;
        let model = crate::turn::conversation_model(&self.runtime, &record).await?;
        let claimed = crate::turn::dispatch::claim_dispatch_inputs(
            &self.runtime,
            &self.conversation_id,
            &cancel,
            &model.info.provider,
        )
        .await?;
        let Some(claimed) = claimed else {
            return Err(AgentError::Cancelled);
        };
        self.synchronize(&claimed.entries, &model.info.provider, context)
            .await?;
        {
            let mut metadata =
                self.projection.metadata.lock().map_err(|_| {
                    AgentError::Store("Durable request metadata lock poisoned".into())
                })?;
            metadata.input_through_seq = claimed.through_seq;
            metadata.goal_id = super::state::active_goal_id(&claimed.entries);
            if claimed.new_submission {
                metadata.continued = false;
            }
        }
        self.runtime
            .note_processed_seq(&self.conversation_id, claimed.through_seq);
        let conversation = harness
            .conversation(ROOT_CONVERSATION_ID, context)
            .await
            .map_err(session_error)?
            .ok_or_else(|| AgentError::Store("Durable root is absent".into()))?;
        Ok(conversation
            .context(context, None)
            .await
            .map_err(session_error)?
            .messages)
    }
}

struct Fingerprint(Sha256);

impl Write for Fingerprint {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fingerprint(messages: &[Message]) -> SessionResult<String> {
    let mut writer = Fingerprint(Sha256::new());
    serde_json::to_writer(&mut writer, messages)
        .map_err(|error| rpi_durable::session::SessionError::message(error.to_string()))?;
    Ok(hex::encode(writer.0.finalize()))
}

fn session_error(error: rpi_durable::session::SessionError) -> AgentError {
    match error {
        rpi_durable::session::SessionError::Aborted => AgentError::Cancelled,
        other => AgentError::Store(other.to_string()),
    }
}
