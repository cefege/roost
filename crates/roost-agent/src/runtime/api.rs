//! The operations the coordinator's RPC handlers call: conversation create,
//! list, transcript, delete, submit, model and thinking changes, settings,
//! and restart recovery.

use std::collections::HashMap;

use roost_llm::{Catalog, THINKING_LEVELS};
use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, ConversationSummary, ModelRef, Transcript, parse_slash_command,
};

use super::{AgentRuntime, Steer, now_ms, random_id};
use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Entry, Mode, Role, model_ref};
use crate::{commands, judging, projection, roles};

const TITLE_CHARS: usize = 60;
const DEFAULT_THINKING: &str = "medium";

/// What a new top-level conversation starts with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewConversation {
    pub worker_fp: String,
    pub worker_label: String,
    pub worker_os: String,
    pub cwd: String,
    pub title: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
}

impl AgentRuntime {
    pub fn catalog(&self) -> &Catalog {
        self.inner.llm.catalog()
    }

    pub async fn create_conversation(
        &self,
        new: NewConversation,
    ) -> Result<ConversationSummary, AgentError> {
        let model = match new.model {
            Some(model) => Some(model),
            None => self.default_model().await,
        };
        let thinking_level = new
            .thinking_level
            .unwrap_or_else(|| DEFAULT_THINKING.to_owned());
        validate_thinking(&thinking_level)?;
        let now = now_ms();
        let record = ConversationRecord {
            id: random_id("c-"),
            title: new.title.unwrap_or_default(),
            worker_fp: new.worker_fp,
            worker_label: new.worker_label,
            worker_os: new.worker_os,
            cwd: new.cwd,
            model,
            thinking_level,
            mode: Mode::Normal,
            pre_plan_model: None,
            parent_id: None,
            agent: None,
            advisor: None,
            run_state: AgentRunState::Idle,
            error: None,
            created_ms: now,
            updated_ms: now,
        };
        self.inner.store.save_conversation(&record).await?;
        tracing::info!(conversation_id = %record.id, worker_fp = %record.worker_fp, "agent conversation created");
        self.publish_summary(&record).await;
        Ok(self.summary(&record).await)
    }

    /// The `default` role's model, if one is available.
    pub async fn default_model(&self) -> Option<ModelRef> {
        let settings = self.inner.store.settings().await.ok()?;
        roles::resolve_role(self.inner.llm.as_ref(), &settings, Role::Default)
            .await
            .map(|model| model_ref(&model.info.provider, &model.info.id))
    }

    /// The thinking levels a client may offer; `auto` only when a judge resolves.
    pub async fn thinking_levels(&self) -> Vec<String> {
        let mut levels: Vec<String> = THINKING_LEVELS
            .iter()
            .map(|level| (*level).to_owned())
            .collect();
        if judging::has_judge(self).await {
            levels.push("auto".into());
        }
        levels
    }

    pub async fn conversations(&self) -> Result<Vec<ConversationSummary>, AgentError> {
        let records = self.inner.store.conversations().await?;
        let mut summaries = Vec::with_capacity(records.len());
        for record in &records {
            summaries.push(self.summary(record).await);
        }
        Ok(summaries)
    }

    pub async fn conversation(&self, id: &str) -> Result<ConversationSummary, AgentError> {
        let record = self.record(id).await?;
        Ok(self.summary(&record).await)
    }

    pub async fn transcript(&self, id: &str) -> Result<Transcript, AgentError> {
        let record = self.record(id).await?;
        let entries = self.inner.store.entries(id).await?;
        let usage = self.usage_totals(id).await?;
        Ok(projection::transcript(&record, &entries, usage))
    }

    /// Aborts the conversation and its children, releases their worker
    /// state, and deletes them.
    pub async fn delete_conversation(&self, id: &str) -> Result<(), AgentError> {
        let records = self.inner.store.conversations().await?;
        let mut doomed = vec![self.record(id).await?];
        let mut cursor = 0;
        while let Some(parent) = doomed.get(cursor).map(|record| record.id.clone()) {
            doomed.extend(
                records
                    .iter()
                    .filter(|record| record.parent_id.as_deref() == Some(parent.as_str()))
                    .cloned(),
            );
            cursor += 1;
        }
        for record in &doomed {
            self.abort(&record.id);
            self.inner
                .tools
                .close_conversation(&record.worker_fp, &record.id)
                .await;
            self.advisor_forget(&record.id);
        }
        self.inner.store.delete_conversation(id).await?;
        for record in &doomed {
            self.inner.sink.removed(&record.id).await;
        }
        tracing::info!(
            conversation_id = id,
            removed = doomed.len(),
            "agent conversation deleted"
        );
        Ok(())
    }

    /// Submits user text: a slash command runs on the coordinator; other text
    /// steers a running conversation or starts a run.
    pub async fn submit(&self, id: &str, text: String) -> Result<(), AgentError> {
        let record = self.record(id).await?;
        if let Some((command, args)) = parse_slash_command(&text) {
            return commands::execute(self, &record, command, args).await;
        }
        if text.trim().is_empty() {
            return Err(AgentError::InvalidArgument("message is empty".into()));
        }
        if record.title.trim().is_empty() {
            let title: String = text.trim().chars().take(TITLE_CHARS).collect();
            self.update_record(id, |row| row.title = title).await?;
        }
        self.deliver_pending_advisories(id).await?;
        self.steer(id, Steer::User(text)).await
    }

    /// Marks every advisory card still waiting as delivered, so it joins the
    /// context of the user message that follows it.
    async fn deliver_pending_advisories(&self, id: &str) -> Result<(), AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let mut latest: HashMap<String, Entry> = HashMap::new();
        let mut order = Vec::new();
        for (_, entry) in entries {
            if let Entry::Advisory { item_id, .. } = &entry {
                if !latest.contains_key(item_id) {
                    order.push(item_id.clone());
                }
                latest.insert(item_id.clone(), entry);
            }
        }
        for item_id in order {
            if let Some(Entry::Advisory {
                severity,
                note,
                delivered: false,
                ..
            }) = latest.remove(&item_id)
            {
                self.append(
                    id,
                    Entry::Advisory {
                        item_id,
                        severity,
                        note,
                        delivered: true,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn set_model(&self, id: &str, model: ModelRef) -> Result<(), AgentError> {
        if self
            .catalog()
            .get(&model.provider, &model.model_id)
            .is_none()
        {
            return Err(AgentError::InvalidArgument(format!(
                "unknown model {}/{}",
                model.provider, model.model_id
            )));
        }
        let record = self
            .update_record(id, |row| row.model = Some(model))
            .await?;
        self.emit_agent(&record).await;
        Ok(())
    }

    /// Renames or moves a conversation to another machine or folder. Worker
    /// state for the old location is released.
    pub async fn set_details(
        &self,
        id: &str,
        title: Option<String>,
        worker: Option<(String, String, String)>,
        cwd: Option<String>,
    ) -> Result<ConversationSummary, AgentError> {
        let before = self.record(id).await?;
        let moved = worker.is_some() || cwd.is_some();
        let record = self
            .update_record(id, |row| {
                if let Some(title) = title {
                    row.title = title;
                }
                if let Some((fp, label, os)) = worker {
                    row.worker_fp = fp;
                    row.worker_label = label;
                    row.worker_os = os;
                }
                if let Some(cwd) = cwd {
                    row.cwd = cwd;
                }
            })
            .await?;
        if moved {
            self.inner
                .tools
                .close_conversation(&before.worker_fp, id)
                .await;
        }
        Ok(self.summary(&record).await)
    }

    pub async fn set_thinking_level(&self, id: &str, level: String) -> Result<(), AgentError> {
        validate_thinking(&level)?;
        let record = self
            .update_record(id, |row| row.thinking_level = level)
            .await?;
        self.emit_agent(&record).await;
        Ok(())
    }

    pub(crate) async fn emit_agent(&self, record: &ConversationRecord) {
        self.emit(
            &record.id,
            vec![ChatEvent::Agent {
                model: record.model.clone(),
                thinking_level: Some(record.thinking_level.clone()),
                mode: Some(record.mode.as_str().to_owned()),
            }],
        )
        .await;
    }

    pub async fn settings(&self) -> Result<AgentSettings, AgentError> {
        self.inner.store.settings().await
    }

    /// Saves settings after checking every role selector's syntax.
    pub async fn set_settings(&self, settings: AgentSettings) -> Result<(), AgentError> {
        for (role, selector) in &settings.model_roles {
            roles::validate_selector(selector).map_err(|error| {
                AgentError::InvalidArgument(format!("role {}: {error}", role.as_str()))
            })?;
        }
        self.inner.store.save_settings(&settings).await?;
        tracing::info!(
            roles = settings.model_roles.len(),
            advisor_enabled = settings.advisor_enabled,
            "agent settings saved"
        );
        for record in self.inner.store.conversations().await? {
            self.publish_summary(&record).await;
        }
        Ok(())
    }

    /// Settles runs a coordinator restart interrupted: the row goes idle and
    /// the transcript says why.
    pub async fn recover_interrupted_runs(&self) -> Result<usize, AgentError> {
        let mut recovered = 0;
        for record in self.inner.store.conversations().await? {
            if record.run_state != AgentRunState::Running || self.is_running(&record.id) {
                continue;
            }
            self.update_record(&record.id, |row| {
                row.run_state = AgentRunState::Idle;
                row.error = None;
            })
            .await?;
            self.append(
                &record.id,
                Entry::Notice {
                    level: "warn".into(),
                    title: "Run interrupted".into(),
                    body: "Run interrupted by a coordinator restart".into(),
                },
            )
            .await?;
            recovered += 1;
        }
        if recovered > 0 {
            tracing::info!(recovered, "interrupted agent runs settled");
        }
        Ok(recovered)
    }
}

fn validate_thinking(level: &str) -> Result<(), AgentError> {
    if level == "auto" || THINKING_LEVELS.contains(&level) {
        Ok(())
    } else {
        Err(AgentError::InvalidArgument(format!(
            "unknown thinking level {level}"
        )))
    }
}
