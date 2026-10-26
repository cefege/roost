//! Per-conversation agent document and resolution
//! (port of `packages/durable/src/harness/agent.ts` @ `abe508e1b`).
//!
//! The `pi.agent` document stores a conversation's *choices*: model, thinking
//! level, extension selection, tool filter, instructions, and working directory.
//! It is rewindable with an `asOf` fork, so forks start from the agent in effect
//! at their fork entry. [`resolve_agent`] composes the stored state against a
//! registry snapshot and the resolved settings.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, LazyLock};

use rpi_ai::types::ModelThinkingLevel;

use crate::chord::json::JsonObject;
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::harness::types::{
    Agent, AgentChange, AgentState, ExtensionsChange, ExtensionsSelection, HookHandlers,
    ModelToolsChange, ModelToolsSelection, PromptSection, ReportFn, Settings, ToolRegistration,
    ToolsChange, ToolsSelection, Wrap, read_state, write_state,
};
use crate::ids::ConversationId;
use crate::session::{DocOwner, SessionResult, Transaction};
use crate::types::{ConversationRecord, DocumentFork, DocumentHistory};

/// Default retry policy (`DEFAULT_RETRY_POLICY`).
pub const DEFAULT_RETRY_POLICY: crate::harness::types::ConversationRetryPolicy =
    crate::harness::types::ConversationRetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 2000,
        max_agent_delay_ms: Some(60_000),
    };

/// Default compaction policy (`DEFAULT_COMPACTION_POLICY`).
pub const DEFAULT_COMPACTION_POLICY: crate::harness::types::CompactionPolicy =
    crate::harness::types::CompactionPolicy {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
        background_tokens: 32_768,
    };

/// Default progress policy (`DEFAULT_PROGRESS_POLICY`).
pub const DEFAULT_PROGRESS_POLICY: crate::harness::types::ProgressPolicy =
    crate::harness::types::ProgressPolicy {
        partial_interval_ms: 100,
        output_interval_ms: 100,
        persist_within_ms: 1_000.0,
    };

/// The reserved section key of the agent's `instructions`.
pub const INSTRUCTIONS_KEY: &str = "instructions";

/// Default `settings.contextRetentionMs`: ten minutes.
pub const DEFAULT_CONTEXT_RETENTION_MS: u64 = 600_000;

fn initial() -> JsonObject {
    JsonObject::new()
}

/// Built-in agent document (`pi.agent`); rewindable with an `asOf` fork.
pub static AGENT_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.agent".to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(DocumentHistory::Rewindable),
        fork: Some(DocumentFork::AsOf),
        family: false,
        initial: Arc::new(|_seed| initial()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_value, _ops, _revision| true)),
    })
});

/// Resolve the host settings: every field over its built-in default, object
/// fields merged.
pub fn resolve_settings(settings: Option<&crate::harness::types::HarnessSettings>) -> Settings {
    let retry = settings.and_then(|settings| settings.retry);
    let compaction = settings.and_then(|settings| settings.compaction);
    let progress = settings.and_then(|settings| settings.progress);
    Settings {
        extensions: settings.and_then(|settings| settings.extensions.clone()),
        stream: settings
            .and_then(|settings| settings.stream.clone())
            .unwrap_or_default(),
        retry: crate::harness::types::ConversationRetryPolicy {
            enabled: retry
                .and_then(|retry| retry.enabled)
                .unwrap_or(DEFAULT_RETRY_POLICY.enabled),
            max_retries: retry
                .and_then(|retry| retry.max_retries)
                .unwrap_or(DEFAULT_RETRY_POLICY.max_retries),
            base_delay_ms: retry
                .and_then(|retry| retry.base_delay_ms)
                .unwrap_or(DEFAULT_RETRY_POLICY.base_delay_ms),
            max_agent_delay_ms: retry
                .and_then(|retry| retry.max_agent_delay_ms)
                .or(DEFAULT_RETRY_POLICY.max_agent_delay_ms),
        },
        compaction: crate::harness::types::CompactionPolicy {
            enabled: compaction
                .and_then(|compaction| compaction.enabled)
                .unwrap_or(DEFAULT_COMPACTION_POLICY.enabled),
            reserve_tokens: compaction
                .and_then(|compaction| compaction.reserve_tokens)
                .unwrap_or(DEFAULT_COMPACTION_POLICY.reserve_tokens),
            keep_recent_tokens: compaction
                .and_then(|compaction| compaction.keep_recent_tokens)
                .unwrap_or(DEFAULT_COMPACTION_POLICY.keep_recent_tokens),
            background_tokens: compaction
                .and_then(|compaction| compaction.background_tokens)
                .unwrap_or(DEFAULT_COMPACTION_POLICY.background_tokens),
        },
        // Field by field, so an explicitly absent interval keeps its default.
        progress: crate::harness::types::ProgressPolicy {
            partial_interval_ms: progress
                .and_then(|progress| progress.partial_interval_ms)
                .unwrap_or(DEFAULT_PROGRESS_POLICY.partial_interval_ms),
            output_interval_ms: progress
                .and_then(|progress| progress.output_interval_ms)
                .unwrap_or(DEFAULT_PROGRESS_POLICY.output_interval_ms),
            persist_within_ms: progress
                .and_then(|progress| progress.persist_within_ms)
                .unwrap_or(DEFAULT_PROGRESS_POLICY.persist_within_ms),
        },
        tool_execution: settings
            .and_then(|settings| settings.tool_execution)
            .unwrap_or(crate::harness::types::ToolExecutionMode::Parallel),
        steering_mode: settings
            .and_then(|settings| settings.steering_mode)
            .unwrap_or(crate::harness::types::QueueMode::OneAtATime),
        follow_up_mode: settings
            .and_then(|settings| settings.follow_up_mode)
            .unwrap_or(crate::harness::types::QueueMode::OneAtATime),
        context_retention_ms: settings
            .and_then(|settings| settings.context_retention_ms)
            .unwrap_or(DEFAULT_CONTEXT_RETENTION_MS),
    }
}

/// Apply one change to `pi.agent`: a given field replaces the stored one, `null`
/// clears it, `undefined` changes nothing.
pub async fn configure(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    change: &AgentChange,
) -> SessionResult<()> {
    let draft = tx
        .doc(&AGENT_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let mut state: AgentState = read_state(&draft)?;
    apply_change(&mut state, change);
    write_state(&draft, &state)
}

/// Apply one [`AgentChange`] to a stored state (pure, in memory).
pub fn apply_change(state: &mut AgentState, change: &AgentChange) {
    if let Some(model) = &change.model {
        state.model = model.clone();
    }
    if let Some(thinking_level) = &change.thinking_level {
        state.thinking_level = *thinking_level;
    }
    if let Some(extensions) = &change.extensions {
        state.extensions = match extensions {
            None => None,
            Some(ExtensionsChange::List(list)) => Some(ExtensionsSelection::List(
                list.iter()
                    .map(|extension| extension.name.clone())
                    .collect(),
            )),
            Some(ExtensionsChange::Edit(edit)) => Some(ExtensionsSelection::Edit {
                add: edit.add.as_ref().map(|list| {
                    list.iter()
                        .map(|extension| extension.name.clone())
                        .collect()
                }),
                remove: edit.remove.as_ref().map(|list| {
                    list.iter()
                        .map(|extension| extension.name.clone())
                        .collect()
                }),
            }),
        };
    }
    if let Some(tools) = &change.tools {
        state.tools = match tools {
            None => None,
            Some(ToolsChange::List(list)) => Some(ToolsSelection::List(
                list.iter().map(|tool| tool.name().to_string()).collect(),
            )),
            Some(ToolsChange::Remove(list)) => Some(ToolsSelection::Remove {
                remove: list.iter().map(|tool| tool.name().to_string()).collect(),
            }),
        };
    }
    if let Some(model_tools) = &change.model_tools {
        state.model_tools = match model_tools {
            None => None,
            Some(ModelToolsChange::List(list)) => Some(ModelToolsSelection::List(
                list.iter().map(|tool| tool.name().to_string()).collect(),
            )),
            Some(ModelToolsChange::Edit(edit)) => Some(ModelToolsSelection::Edit {
                add: edit
                    .add
                    .as_ref()
                    .map(|list| list.iter().map(|tool| tool.name().to_string()).collect()),
                remove: edit
                    .remove
                    .as_ref()
                    .map(|list| list.iter().map(|tool| tool.name().to_string()).collect()),
            }),
        };
    }
    if let Some(instructions) = &change.instructions {
        state.instructions = instructions.clone();
    }
    if let Some(cwd) = &change.cwd {
        state.cwd = cwd.clone();
    }
}

/// Apply `addTools` to enabled calls and the model's deferred-load set.
pub async fn add_tools(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    added: &[String],
) -> SessionResult<()> {
    let draft = tx
        .doc(&AGENT_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let mut state: AgentState = read_state(&draft)?;
    match &mut state.tools {
        None => {}
        Some(ToolsSelection::List(list)) => {
            for name in added {
                if !list.contains(name) {
                    list.push(name.clone());
                }
            }
        }
        Some(ToolsSelection::Remove { remove }) => {
            remove.retain(|name| !added.contains(name));
        }
    }
    match &mut state.model_tools {
        Some(ModelToolsSelection::List(list)) => {
            for name in added {
                if !list.contains(name) {
                    list.push(name.clone());
                }
            }
        }
        Some(ModelToolsSelection::Edit { add, remove }) => {
            if let Some(remove) = remove {
                remove.retain(|name| !added.contains(name));
            }
            let add = add.get_or_insert_with(Vec::new);
            for name in added {
                if !add.contains(name) {
                    add.push(name.clone());
                }
            }
        }
        None => {
            state.model_tools = Some(ModelToolsSelection::Edit {
                add: Some(added.to_vec()),
                remove: None,
            });
        }
    }
    write_state(&draft, &state)
}

/// Built-in part of every commit that creates or forks a conversation, for
/// `pi.agent`: a fork keeps its `asOf` copy; a new task-owned conversation copies
/// its owner's agent; a new ownerless one starts empty.
pub async fn create_agent(
    tx: &mut Transaction,
    conversation: &ConversationRecord,
) -> SessionResult<()> {
    if conversation.parent.is_some() {
        return Ok(());
    }
    let agent = tx
        .doc(&AGENT_DOC, DocOwner::Conversation(conversation.id), None)
        .await?;
    let Some(owner) = &conversation.owner else {
        return Ok(());
    };
    let owner_agent = tx
        .doc(
            &AGENT_DOC,
            DocOwner::Conversation(owner.conversation_id),
            None,
        )
        .await?;
    let value = owner_agent.read()?;
    let mut guard = agent.write()?;
    *guard = value;
    Ok(())
}

/// Handlers of the selected extensions' hooks for a task name, in extension order.
pub fn agent_hooks(agent: &Agent, task_name: &str) -> Vec<HookHandlers> {
    let mut handlers = Vec::new();
    for extension in &agent.extensions {
        for hook in &extension.hooks {
            if hook.task == task_name {
                handlers.push(hook.handlers.clone());
            }
        }
    }
    handlers
}

/// Resolve an agent from its stored state, a registry snapshot, and resolved
/// settings. A wrapper that throws or renames drops its target and is reported; a
/// wrapper without a target does nothing.
pub fn resolve_agent(
    state: Option<&AgentState>,
    snapshot: &dyn crate::harness::types::RegistrySnapshot,
    settings: &Settings,
    report: &ReportFn,
) -> Agent {
    let extensions = select_extensions(
        state.and_then(|state| state.extensions.as_ref()),
        snapshot,
        settings,
    );

    let mut composed: Vec<(String, ToolRegistration)> = Vec::new();
    for extension in &extensions {
        for tool in &extension.tools {
            upsert(&mut composed, tool.name().to_string(), tool.clone());
        }
    }
    let mut sections: Vec<(String, PromptSection)> = Vec::new();
    for extension in &extensions {
        for section in &extension.sections {
            upsert(&mut sections, section.key.clone(), section.clone());
        }
    }
    for extension in &extensions {
        for wrap in &extension.wraps {
            match wrap {
                Wrap::Tool { tool, wrap } => apply_wrap_tool(&mut composed, tool, wrap, report),
                Wrap::Section { key, wrap } => apply_wrap_section(&mut sections, key, wrap, report),
            }
        }
    }

    let enabled = select_enabled_tools(&composed, state.and_then(|state| state.tools.as_ref()));
    let model_callable: Vec<ToolRegistration> = enabled
        .iter()
        .filter(|tool| tool.available_to(crate::harness::types::ToolCaller::Model))
        .cloned()
        .collect();
    let tools = select_model_tools(
        &model_callable,
        state.and_then(|state| state.model_tools.as_ref()),
    );
    let offered: HashSet<&str> = tools.iter().map(|tool| tool.name()).collect();
    let deferred = model_callable
        .iter()
        .filter(|tool| tool.deferred && !offered.contains(tool.name()))
        .cloned()
        .collect();
    let callable = enabled
        .into_iter()
        .filter(|tool| tool.available_to(crate::harness::types::ToolCaller::Tool))
        .collect();

    let instructions = state.and_then(|state| state.instructions.clone());
    let mut agent_sections: Vec<PromptSection> =
        sections.into_iter().map(|(_, section)| section).collect();
    if let Some(text) = &instructions {
        let body = text.clone();
        agent_sections.push(PromptSection {
            key: INSTRUCTIONS_KEY.to_string(),
            render: Arc::new(move |_input, _context| {
                let body = body.clone();
                Box::pin(async move { Some(body) })
            }),
            tag: true,
        });
    }

    Agent {
        model: state.and_then(|state| state.model.clone()),
        thinking_level: state
            .and_then(|state| state.thinking_level)
            .unwrap_or(ModelThinkingLevel::Off),
        extensions,
        tools,
        callable,
        deferred,
        sections: agent_sections,
        instructions,
        cwd: state.and_then(|state| state.cwd.clone()),
    }
}

fn select_enabled_tools(
    composed: &[(String, ToolRegistration)],
    selection: Option<&ToolsSelection>,
) -> Vec<ToolRegistration> {
    match selection {
        None => composed.iter().map(|(_, tool)| tool.clone()).collect(),
        Some(ToolsSelection::List(list)) => {
            let mut seen = HashSet::new();
            list.iter()
                .filter_map(|name| {
                    seen.insert(name.as_str())
                        .then(|| {
                            composed
                                .iter()
                                .find(|(key, _)| key == name)
                                .map(|(_, tool)| tool.clone())
                        })
                        .flatten()
                })
                .collect()
        }
        Some(ToolsSelection::Remove { remove }) => {
            let removed: HashSet<&str> = remove.iter().map(String::as_str).collect();
            composed
                .iter()
                .filter(|(name, _)| !removed.contains(name.as_str()))
                .map(|(_, tool)| tool.clone())
                .collect()
        }
    }
}

fn select_model_tools(
    tools: &[ToolRegistration],
    selection: Option<&ModelToolsSelection>,
) -> Vec<ToolRegistration> {
    match selection {
        Some(ModelToolsSelection::List(list)) => {
            let mut seen = HashSet::new();
            list.iter()
                .filter_map(|name| {
                    seen.insert(name.as_str())
                        .then(|| tools.iter().find(|tool| tool.name() == name).cloned())
                        .flatten()
                })
                .collect()
        }
        None => tools
            .iter()
            .filter(|tool| !tool.deferred)
            .cloned()
            .collect(),
        Some(ModelToolsSelection::Edit { add, remove }) => {
            let removed: HashSet<&str> = remove
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(String::as_str)
                .collect();
            let mut offered: Vec<ToolRegistration> = tools
                .iter()
                .filter(|tool| !tool.deferred && !removed.contains(tool.name()))
                .cloned()
                .collect();
            let mut seen: HashSet<String> =
                offered.iter().map(|tool| tool.name().to_string()).collect();
            for name in add.iter().flatten() {
                if !removed.contains(name.as_str())
                    && seen.insert(name.clone())
                    && let Some(tool) = tools
                        .iter()
                        .find(|tool| tool.deferred && tool.name() == name)
                {
                    offered.push(tool.clone());
                }
            }
            offered
        }
    }
}

/// Selected installed extensions: the stored array, or the default selection
/// edited by `{ add, remove }`.
pub fn select_extensions(
    stored: Option<&ExtensionsSelection>,
    snapshot: &dyn crate::harness::types::RegistrySnapshot,
    settings: &Settings,
) -> Vec<Arc<crate::harness::types::Extension>> {
    let selected: Vec<String> = match stored {
        Some(ExtensionsSelection::List(list)) => list.clone(),
        other => {
            let base: Vec<String> = match &settings.extensions {
                Some(extensions) => extensions
                    .iter()
                    .map(|extension| extension.name.clone())
                    .collect(),
                None => snapshot
                    .installed()
                    .iter()
                    .map(|extension| extension.name.clone())
                    .collect(),
            };
            let (add, remove) = match other {
                Some(ExtensionsSelection::Edit { add, remove }) => (
                    add.clone().unwrap_or_default(),
                    remove.clone().unwrap_or_default(),
                ),
                _ => (Vec::new(), Vec::new()),
            };
            let removed: BTreeSet<String> = remove.into_iter().collect();
            base.into_iter()
                .chain(add)
                .filter(|name| !removed.contains(name))
                .collect()
        }
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut extensions = Vec::new();
    for name in selected {
        if seen.insert(name.clone())
            && let Some(extension) = snapshot.extension(&name)
        {
            extensions.push(extension);
        }
    }
    extensions
}

fn upsert<T>(items: &mut Vec<(String, T)>, key: String, value: T) {
    match items.iter().position(|(name, _)| name == &key) {
        Some(index) => items[index].1 = value,
        None => items.push((key, value)),
    }
}

fn apply_wrap_tool(
    items: &mut Vec<(String, ToolRegistration)>,
    target: &str,
    wrap: &Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
    report: &ReportFn,
) {
    let Some(index) = items.iter().position(|(name, _)| name == target) else {
        return;
    };
    let wrapped = wrap(items[index].1.clone());
    if wrapped.name() != target {
        let renamed = wrapped.name().to_string();
        items.remove(index);
        report(format!("Wrapper renamed {target} to {renamed}"));
    } else {
        items[index].1 = wrapped;
    }
}

fn apply_wrap_section(
    items: &mut Vec<(String, PromptSection)>,
    target: &str,
    wrap: &Arc<dyn Fn(PromptSection) -> PromptSection + Send + Sync>,
    report: &ReportFn,
) {
    let Some(index) = items.iter().position(|(key, _)| key == target) else {
        return;
    };
    let wrapped = wrap(items[index].1.clone());
    if wrapped.key != target {
        let renamed = wrapped.key.clone();
        items.remove(index);
        report(format!("Wrapper renamed {target} to {renamed}"));
    } else {
        items[index].1 = wrapped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::context::Context;
    use crate::chord::json::JsonValue;
    use crate::harness::registry::RegistryImpl;
    use crate::harness::types::{
        ExtensionsEdit, ModelToolsEdit, PromptInput, PromptRender, PromptRenderFuture, Registry,
        RegistryReader, RegistrySnapshot, ToolExecute, ToolExecuteError, ToolExecutionApi,
        ToolExecutionResult, ToolReplay,
    };
    use rpi_ai::types::Tool;
    use std::sync::Mutex;

    struct NoopTool;

    #[async_trait::async_trait]
    impl ToolExecute for NoopTool {
        async fn execute(
            &self,
            _args: JsonObject,
            _api: &dyn ToolExecutionApi,
            _context: &Context,
        ) -> Result<ToolExecutionResult, ToolExecuteError> {
            Ok(ToolExecutionResult::default())
        }
    }

    fn tool(name: &str, description: &str) -> ToolRegistration {
        ToolRegistration {
            tool: Tool {
                name: name.to_string(),
                description: description.to_string(),
                parameters: serde_json::json!({ "type": "object" }),
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            deferred: false,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            output_limits: None,
            execute: Arc::new(NoopTool),
        }
    }

    fn render_body(body: &'static str) -> PromptRender {
        Arc::new(
            move |_input: PromptInput, _context: Context| -> PromptRenderFuture {
                Box::pin(async move { Some(body.to_string()) })
            },
        )
    }

    fn snapshot(extensions: Vec<crate::harness::types::Extension>) -> Arc<dyn RegistrySnapshot> {
        let registry = RegistryImpl::with_builtins(Vec::new());
        for extension in extensions {
            registry.install(extension).unwrap();
        }
        registry.snapshot()
    }

    fn names(agent: &Agent) -> Vec<String> {
        agent
            .extensions
            .iter()
            .map(|extension| extension.name.clone())
            .collect()
    }

    #[test]
    fn resolve_settings_merges_over_defaults() {
        let settings = crate::harness::types::HarnessSettings {
            retry: Some(crate::harness::types::ConversationRetryPolicyPatch {
                max_retries: Some(9),
                ..Default::default()
            }),
            progress: Some(crate::harness::types::ProgressPolicyPatch {
                partial_interval_ms: Some(5),
                output_interval_ms: None,
                persist_within_ms: None,
            }),
            tool_execution: Some(crate::harness::types::ToolExecutionMode::Sequential),
            ..Default::default()
        };
        let resolved = resolve_settings(Some(&settings));
        assert_eq!(resolved.retry.max_retries, 9);
        assert_eq!(
            resolved.retry.base_delay_ms,
            DEFAULT_RETRY_POLICY.base_delay_ms
        );
        assert_eq!(resolved.progress.partial_interval_ms, 5);
        assert_eq!(
            resolved.progress.output_interval_ms,
            DEFAULT_PROGRESS_POLICY.output_interval_ms
        );
        assert_eq!(
            resolved.tool_execution,
            crate::harness::types::ToolExecutionMode::Sequential
        );
        assert_eq!(resolved.context_retention_ms, DEFAULT_CONTEXT_RETENTION_MS);
    }

    #[test]
    fn apply_change_replaces_clears_and_leaves_alone() {
        let mut state = AgentState {
            instructions: Some("keep".to_string()),
            cwd: Some("/tmp".to_string()),
            ..AgentState::default()
        };
        apply_change(
            &mut state,
            &AgentChange {
                thinking_level: Some(Some(ModelThinkingLevel::High)),
                cwd: Some(None), // clear
                model: None,     // leave alone
                ..AgentChange::default()
            },
        );
        assert_eq!(state.thinking_level, Some(ModelThinkingLevel::High));
        assert_eq!(state.cwd, None);
        assert_eq!(state.instructions.as_deref(), Some("keep"));
    }

    #[test]
    fn apply_change_encodes_extensions_and_tools() {
        let mut state = AgentState::default();
        apply_change(
            &mut state,
            &AgentChange {
                extensions: Some(Some(ExtensionsChange::Edit(ExtensionsEdit {
                    add: Some(vec![Arc::new(crate::harness::types::Extension {
                        name: "added".to_string(),
                        ..Default::default()
                    })]),
                    remove: Some(vec![Arc::new(crate::harness::types::Extension {
                        name: "gone".to_string(),
                        ..Default::default()
                    })]),
                }))),
                tools: Some(Some(ToolsChange::Remove(vec![tool("x", "")]))),
                model_tools: Some(Some(ModelToolsChange::Edit(ModelToolsEdit {
                    add: Some(vec![tool("lazy", "")]),
                    remove: Some(vec![tool("read", "")]),
                }))),
                ..AgentChange::default()
            },
        );
        assert_eq!(
            state.extensions,
            Some(ExtensionsSelection::Edit {
                add: Some(vec!["added".to_string()]),
                remove: Some(vec!["gone".to_string()]),
            })
        );
        assert_eq!(
            state.tools,
            Some(ToolsSelection::Remove {
                remove: vec!["x".to_string()]
            })
        );
        assert_eq!(
            state.model_tools,
            Some(ModelToolsSelection::Edit {
                add: Some(vec!["lazy".to_string()]),
                remove: Some(vec!["read".to_string()]),
            })
        );
    }

    #[test]
    fn resolve_agent_selects_edits_and_applies_filters() {
        let settings = resolve_settings(None);
        let snapshot = snapshot(vec![
            crate::harness::types::Extension {
                name: "a".to_string(),
                tools: vec![tool("x", "from a"), tool("y", "y")],
                ..Default::default()
            },
            crate::harness::types::Extension {
                name: "b".to_string(),
                tools: vec![tool("x", "from b")],
                ..Default::default()
            },
        ]);
        let report: crate::harness::types::ReportFn = Arc::new(|_| {});

        // A stored array selects exactly these extensions.
        let state = AgentState {
            extensions: Some(ExtensionsSelection::List(vec!["b".to_string()])),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert_eq!(names(&agent), vec!["b"]);
        assert_eq!(agent.tools.len(), 1);
        assert_eq!(agent.tools[0].tool.description, "from b");

        // An edit removes from the default selection.
        let state = AgentState {
            extensions: Some(ExtensionsSelection::Edit {
                add: None,
                remove: Some(vec!["a".to_string()]),
            }),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert_eq!(names(&agent), vec!["b"]);

        // A tool list offers exactly those tools; an unknown name is skipped.
        let state = AgentState {
            tools: Some(ToolsSelection::List(vec![
                "y".to_string(),
                "missing".to_string(),
            ])),
            instructions: Some("be brief".to_string()),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert_eq!(
            agent
                .tools
                .iter()
                .map(|t| t.name().to_string())
                .collect::<Vec<_>>(),
            vec!["y"]
        );
        assert_eq!(
            agent.sections.last().map(|s| s.key.as_str()),
            Some(INSTRUCTIONS_KEY)
        );
        assert_eq!(agent.instructions.as_deref(), Some("be brief"));
        assert_eq!(agent.thinking_level, ModelThinkingLevel::Off);
    }

    #[test]
    fn model_tools_offer_defaults_and_load_deferred_tools_separately() {
        let mut deferred = tool("lazy", "deferred tool");
        deferred.deferred = true;
        let snapshot = snapshot(vec![crate::harness::types::Extension {
            name: "tools".to_string(),
            tools: vec![tool("read", "ordinary tool"), deferred],
            ..Default::default()
        }]);
        let settings = resolve_settings(None);
        let report: crate::harness::types::ReportFn = Arc::new(|_| {});
        let agent = resolve_agent(None, &*snapshot, &settings, &report);
        assert_eq!(names(&agent), vec!["read"]);
        assert_eq!(
            agent
                .callable
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["read", "lazy"],
        );
        assert_eq!(
            agent
                .deferred
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            vec!["lazy"],
        );

        let state = AgentState {
            model_tools: Some(ModelToolsSelection::Edit {
                add: Some(vec!["lazy".to_string()]),
                remove: None,
            }),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert_eq!(names(&agent), vec!["read", "lazy"]);
        assert!(agent.deferred.is_empty());
        assert_eq!(agent.callable.len(), 2);
    }

    #[test]
    fn resolve_agent_applies_wrappers_and_drops_renames() {
        let settings = resolve_settings(None);
        let snapshot = snapshot(vec![crate::harness::types::Extension {
            name: "a".to_string(),
            tools: vec![tool("x", "plain")],
            sections: vec![crate::harness::define::section(
                "prompt",
                render_body("body"),
                None,
            )],
            ..Default::default()
        }]);
        let reports = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&reports);
        let report: crate::harness::types::ReportFn =
            Arc::new(move |message| captured.lock().unwrap().push(message));

        let state = AgentState {
            extensions: Some(ExtensionsSelection::List(vec!["a".to_string()])),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert_eq!(agent.tools[0].tool.description, "plain");
        assert!(agent.sections.iter().any(|s| s.key == "prompt"));

        // A wrapper that renames its target drops it and reports.
        let extension = crate::harness::types::Extension {
            name: "a".to_string(),
            tools: vec![tool("x", "plain")],
            wraps: vec![Wrap::Tool {
                tool: "x".to_string(),
                wrap: Arc::new(|tool: ToolRegistration| {
                    let mut renamed = tool;
                    renamed.tool.name = "z".to_string();
                    renamed
                }),
            }],
            ..Default::default()
        };
        let registry = RegistryImpl::with_builtins(Vec::new());
        registry.install(extension).unwrap();
        let snapshot = registry.snapshot();
        let state = AgentState {
            extensions: Some(ExtensionsSelection::List(vec!["a".to_string()])),
            ..AgentState::default()
        };
        let agent = resolve_agent(Some(&state), &*snapshot, &settings, &report);
        assert!(agent.tools.is_empty());
        assert_eq!(reports.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn configure_add_tools_and_create_agent_write_the_document() {
        use crate::session::{ConversationOwnership, commit_fn, create_session};
        use crate::storage::MemoryStorage;

        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let context = crate::chord::context::BACKGROUND_CONTEXT.clone();
        let conversation = session
            .commit(
                commit_fn(|tx| {
                    Box::pin(async move {
                        let record = tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?;
                        create_agent(tx, &record).await?;
                        Ok(record.id)
                    })
                }),
                &context,
            )
            .await
            .unwrap();
        session
            .commit(
                commit_fn(move |tx| {
                    Box::pin(async move {
                        configure(
                            tx,
                            conversation,
                            &AgentChange {
                                instructions: Some(Some("hello".to_string())),
                                tools: Some(Some(ToolsChange::List(vec![tool("x", "")]))),
                                ..AgentChange::default()
                            },
                        )
                        .await?;
                        add_tools(tx, conversation, &["x".to_string(), "y".to_string()]).await?;
                        Ok(())
                    })
                }),
                &context,
            )
            .await
            .unwrap();
        let value = session
            .snapshot(&AGENT_DOC, DocOwner::Conversation(conversation), &context)
            .await
            .unwrap()
            .unwrap();
        let state: AgentState = serde_json::from_value(JsonValue::Object(value)).unwrap();
        assert_eq!(state.instructions.as_deref(), Some("hello"));
        assert_eq!(
            state.tools,
            Some(ToolsSelection::List(vec!["x".to_string(), "y".to_string()]))
        );
        assert_eq!(
            state.model_tools,
            Some(ModelToolsSelection::Edit {
                add: Some(vec!["x".to_string(), "y".to_string()]),
                remove: None,
            })
        );
    }
}
