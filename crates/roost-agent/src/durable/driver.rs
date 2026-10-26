//! One leased native conversation driven by Pi Durable's real task engine.
//! The outer runtime retains Roost policies and supervision, not a second model
//! loop. Each owner opens the confirmed SQL log and closes before lease release.

mod branch;
mod compaction;
mod generation;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rpi_durable::harness::types::CompactionPolicyPatch;
use rpi_durable::harness::{
    AgentChange, ConversationInit, HarnessImpl, HarnessOptions, HarnessSettings, HookHandlers,
    HookRegistration, Registry, RegistryImpl, RootOptions, create_registry, open_harness,
};
use rpi_durable::session::{DocOwner, SessionError};
use rpi_durable::{BACKGROUND_CONTEXT, ROOT_CONVERSATION_ID, Storage};

use super::hooks::NativeHooks;
use super::journal_sync::JournalSync;
use super::state::ProjectionState;
use super::storage::JournalStorage;
use crate::error::AgentError;
use crate::runtime::AgentRuntime;

pub(crate) struct DurableDriver {
    runtime: AgentRuntime,
    conversation_id: String,
    harness: Arc<HarnessImpl>,
    registry: Arc<RegistryImpl>,
    storage: Arc<JournalStorage>,
    sync: Arc<JournalSync>,
    projection: Arc<ProjectionState>,
    live: super::live::LiveView,
    closed: AtomicBool,
}

impl DurableDriver {
    pub async fn open(runtime: AgentRuntime, conversation_id: String) -> Result<Self, AgentError> {
        let fence = runtime
            .held_lease(&conversation_id)
            .ok_or_else(|| AgentError::Fenced(conversation_id.clone()))?;
        let projection = Arc::new(ProjectionState::new());
        let storage = JournalStorage::open(
            runtime.clone(),
            conversation_id.clone(),
            fence,
            Arc::clone(&projection),
        )
        .await?;
        let registry = Arc::new(create_registry());
        let sync = JournalSync::new(
            runtime.clone(),
            conversation_id.clone(),
            Arc::clone(&projection),
        );
        let hooks = Arc::new(NativeHooks {
            runtime: runtime.clone(),
            conversation_id: conversation_id.clone(),
            sync: Arc::clone(&sync),
            projection: Arc::clone(&projection),
            registry: Arc::downgrade(&registry),
        });
        install_extension(&runtime, &conversation_id, &registry, &projection, hooks).await?;
        let options = HarnessOptions {
            models: super::model::models(
                runtime.clone(),
                conversation_id.clone(),
                Arc::clone(&projection),
            )?,
            registry: registry.clone(),
            settings: Some(HarnessSettings {
                compaction: Some(CompactionPolicyPatch {
                    reserve_tokens: Some(crate::compaction::HEADROOM_TOKENS),
                    keep_recent_tokens: Some(crate::compaction::KEEP_TOKENS),
                    ..CompactionPolicyPatch::default()
                }),
                ..HarnessSettings::default()
            }),
            env: None,
            conversation_created: None,
            now: None,
            on_report: Some(Arc::new(|error| {
                tracing::error!(error, "durable extension reported failure")
            })),
            read_only: false,
            set_timer: None,
            on_wake: None,
        };
        let harness = open_harness(
            storage.clone() as Arc<dyn Storage>,
            options,
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(session_error)?;
        if let Err(error) = sync.bind(&harness) {
            let _ = harness.close(&BACKGROUND_CONTEXT).await;
            return Err(error);
        }
        let identity = conversation_id.clone();
        let init: ConversationInit = Arc::new(move |tx, id| {
            let identity = identity.clone();
            Box::pin(async move {
                let provider = tx
                    .doc(
                        &rpi_durable::harness::PROVIDER_DOC,
                        DocOwner::Conversation(id),
                        None,
                    )
                    .await?;
                *provider.write()? = serde_json::json!({"sessionId": identity});
                Ok(())
            })
        });
        if let Err(error) = harness
            .root(
                &BACKGROUND_CONTEXT,
                RootOptions {
                    agent: Some(AgentChange::default()),
                    init: Some(init),
                },
            )
            .await
        {
            let _ = harness.close(&BACKGROUND_CONTEXT).await;
            return Err(session_error(error));
        }
        let live = match super::live::LiveView::open(
            runtime.clone(),
            conversation_id.clone(),
            Arc::clone(&harness),
            Arc::clone(&projection),
        )
        .await
        {
            Ok(live) => live,
            Err(error) => {
                let _ = harness.close(&BACKGROUND_CONTEXT).await;
                return Err(error);
            }
        };
        tracing::info!(conversation_id, "Pi Durable native conversation opened");
        Ok(Self {
            runtime,
            conversation_id,
            harness,
            registry,
            storage,
            sync,
            projection,
            live,
            closed: AtomicBool::new(false),
        })
    }

    pub async fn close(&self, abort: bool) -> Result<(), AgentError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let aborted = async {
            if abort {
                let root = self
                    .harness
                    .conversation(ROOT_CONVERSATION_ID, &BACKGROUND_CONTEXT)
                    .await
                    .map_err(session_error)?;
                if let Some(root) = root {
                    root.abort(
                        &BACKGROUND_CONTEXT,
                        rpi_durable::harness::ConversationAbortOptions { background: true },
                    )
                    .await
                    .map_err(session_error)?;
                }
            }
            Ok(())
        }
        .await;
        self.live.close().await;
        let closed = self
            .harness
            .close(&BACKGROUND_CONTEXT)
            .await
            .map_err(session_error);
        self.closed.store(true, Ordering::Release);
        aborted.and(closed)
    }

    async fn refresh_extension(&self) -> Result<(), AgentError> {
        install_extension(
            &self.runtime,
            &self.conversation_id,
            &self.registry,
            &self.projection,
            Arc::new(NativeHooks {
                runtime: self.runtime.clone(),
                conversation_id: self.conversation_id.clone(),
                sync: Arc::clone(&self.sync),
                projection: Arc::clone(&self.projection),
                registry: Arc::downgrade(&self.registry),
            }),
        )
        .await
    }
}

impl Drop for DurableDriver {
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(executor) = tokio::runtime::Handle::try_current() {
            let harness = Arc::clone(&self.harness);
            executor.spawn(async move {
                if let Err(error) = harness.close(&BACKGROUND_CONTEXT).await {
                    tracing::warn!(%error, "abandoned Durable owner closed with an error");
                }
            });
        }
    }
}

pub(super) async fn install_extension(
    runtime: &AgentRuntime,
    conversation_id: &str,
    registry: &RegistryImpl,
    projection: &Arc<ProjectionState>,
    hooks: Arc<NativeHooks>,
) -> Result<(), AgentError> {
    let extension = super::tools::extension(
        runtime.clone(),
        conversation_id.to_owned(),
        Arc::clone(&projection.end),
    )
    .await?;
    let mut extension = (*extension).clone();
    extension.hooks.push(HookRegistration {
        task: rpi_durable::harness::GENERATION_TASK_NAME.into(),
        handlers: HookHandlers::Generation(hooks),
    });
    extension.hooks.push(HookRegistration {
        task: rpi_durable::harness::COMPACTION_TASK_NAME.into(),
        handlers: HookHandlers::Compaction(super::compaction::hooks(
            runtime.clone(),
            conversation_id.to_owned(),
        )),
    });
    registry
        .install(extension)
        .map_err(|error| AgentError::InvalidArgument(error.to_string()))
}

fn session_error(error: SessionError) -> AgentError {
    match error {
        SessionError::Aborted => AgentError::Cancelled,
        other => AgentError::Store(other.to_string()),
    }
}
