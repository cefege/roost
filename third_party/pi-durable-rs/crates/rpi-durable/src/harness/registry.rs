//! Extension registry (port of `packages/durable/src/harness/registry.ts`
//! @ `abe508e1b`).
//!
//! A registry publishes immutable [`RegistryState`] snapshots. Installation
//! replaces an extension with the same name in place; removal of a later install
//! appends. Subscribe callbacks run synchronously after each publication so the
//! scheduler can reconsider blocked tasks.
//!
//! The built-in task definitions (`pi.generation`, `pi.tool`, `pi.compaction`)
//! are supplied as a parameter ([`RegistryImpl::with_builtins`]); the module
//! reserves their names so no extension can claim them. [`builtin_tasks`]
//! returns the real definitions, and [`create_registry`] installs them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::harness::agent::INSTRUCTIONS_KEY;
use crate::harness::types::{
    AnyTask, Extension, Registry, RegistryError, RegistryListener, RegistryReader,
    RegistrySnapshot, SectionRef, ToolRef,
};
use crate::session::Subscription;

/// Built-in task name of the generation pipeline.
pub const GENERATION_TASK_NAME: &str = "pi.generation";
/// Built-in task name of tool execution.
pub const TOOL_TASK_NAME: &str = "pi.tool";
/// Built-in task name of compaction.
pub const COMPACTION_TASK_NAME: &str = "pi.compaction";

/// Reserved built-in task names every registry holds (upstream `BUILTIN_TASKS`).
// FORM: upstream exports the task definition objects; Rust reserves their names
// here and takes the definitions as registry parameters.
pub const BUILTIN_TASK_NAMES: [&str; 3] =
    [GENERATION_TASK_NAME, TOOL_TASK_NAME, COMPACTION_TASK_NAME];

const SECTION_KEY_PATTERN: &str = "^[a-z][a-z0-9_-]*$";

fn valid_section_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Unique tool names and section keys within one extension; valid, unreserved
/// section keys.
pub fn validate_extension(extension: &Extension) -> Result<(), RegistryError> {
    let mut tools: BTreeSet<&str> = BTreeSet::new();
    for tool in &extension.tools {
        if !tools.insert(tool.name()) {
            return Err(RegistryError::DuplicateToolName {
                extension: extension.name.clone(),
                name: tool.name().to_string(),
            });
        }
    }
    let mut sections: BTreeSet<&str> = BTreeSet::new();
    for section in &extension.sections {
        if !valid_section_key(&section.key) {
            return Err(RegistryError::InvalidSectionKey {
                key: section.key.clone(),
                pattern: SECTION_KEY_PATTERN,
            });
        }
        if section.key == INSTRUCTIONS_KEY {
            return Err(RegistryError::ReservedSectionKey {
                key: section.key.clone(),
            });
        }
        if !sections.insert(section.key.as_str()) {
            return Err(RegistryError::DuplicateSectionKey {
                extension: extension.name.clone(),
                key: section.key.clone(),
            });
        }
    }
    Ok(())
}

/// Immutable published registry state.
pub struct RegistryState {
    extensions: Vec<Arc<Extension>>,
    by_name: BTreeMap<String, Arc<Extension>>,
    tasks: BTreeMap<String, AnyTask>,
}

impl RegistryState {
    fn empty() -> Self {
        Self {
            extensions: Vec::new(),
            by_name: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }

    /// Build and validate the next immutable state.
    pub fn new(
        extensions: Vec<Arc<Extension>>,
        builtins: Vec<AnyTask>,
    ) -> Result<Self, RegistryError> {
        for extension in &extensions {
            validate_extension(extension)?;
        }
        let mut by_name = BTreeMap::new();
        for extension in &extensions {
            by_name.insert(extension.name.clone(), Arc::clone(extension));
        }
        let mut reserved: BTreeSet<String> = BUILTIN_TASK_NAMES
            .iter()
            .map(|name| name.to_string())
            .collect();
        let mut tasks = BTreeMap::new();
        for builtin in builtins {
            let name = builtin.name().to_string();
            reserved.insert(name.clone());
            tasks.insert(name, builtin);
        }
        for extension in &extensions {
            for task in &extension.tasks {
                let name = task.name().to_string();
                if reserved.contains(&name) {
                    return Err(RegistryError::DuplicateTaskName {
                        name,
                        extension: extension.name.clone(),
                    });
                }
                reserved.insert(name.clone());
                tasks.insert(name, Arc::clone(task));
            }
        }
        Ok(Self {
            extensions,
            by_name,
            tasks,
        })
    }
}

impl RegistrySnapshot for RegistryState {
    fn installed(&self) -> Vec<Arc<Extension>> {
        self.extensions.clone()
    }

    fn extension(&self, name: &str) -> Option<Arc<Extension>> {
        self.by_name.get(name).cloned()
    }

    fn tools(&self) -> Vec<ToolRef> {
        self.extensions
            .iter()
            .flat_map(|extension| {
                extension.tools.iter().map(|tool| ToolRef {
                    extension: Arc::clone(extension),
                    tool: tool.clone(),
                })
            })
            .collect()
    }

    fn sections(&self) -> Vec<SectionRef> {
        self.extensions
            .iter()
            .flat_map(|extension| {
                extension.sections.iter().map(|section| SectionRef {
                    extension: Arc::clone(extension),
                    section: section.clone(),
                })
            })
            .collect()
    }

    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.values().cloned().collect()
    }

    fn task(&self, name: &str) -> Option<AnyTask> {
        self.tasks.get(name).cloned()
    }
}

/// Application-owned registry of extensions.
pub struct RegistryImpl {
    current: Arc<Mutex<Arc<RegistryState>>>,
    builtins: Vec<AnyTask>,
    listeners: Arc<Mutex<Vec<(u64, RegistryListener)>>>,
    next_listener: AtomicU64,
}

impl RegistryImpl {
    /// A registry holding only the given built-in task definitions.
    pub fn with_builtins(builtins: Vec<AnyTask>) -> Self {
        // Invariant: an empty extension list cannot fail validation, so the
        // fallback is unreachable; it avoids `expect` in non-test code.
        let state = RegistryState::new(Vec::new(), builtins.clone())
            .unwrap_or_else(|_| RegistryState::empty());
        Self {
            current: Arc::new(Mutex::new(Arc::new(state))),
            builtins,
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener: AtomicU64::new(1),
        }
    }

    fn publish(&self, extensions: Vec<Arc<Extension>>) -> Result<(), RegistryError> {
        let state = RegistryState::new(extensions, self.builtins.clone())?;
        if let Ok(mut current) = self.current.lock() {
            *current = Arc::new(state);
        }
        let listeners: Vec<RegistryListener> = match self.listeners.lock() {
            Ok(listeners) => listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect(),
            Err(_) => return Ok(()),
        };
        for listener in listeners {
            listener();
        }
        Ok(())
    }
}

impl RegistryReader for RegistryImpl {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        match self.current.lock() {
            Ok(current) => Arc::clone(&current) as Arc<dyn RegistrySnapshot>,
            Err(_) => Arc::new(RegistryState::empty()),
        }
    }

    fn subscribe(&self, listener: RegistryListener) -> Subscription {
        let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push((id, listener));
        }
        let listeners = Arc::clone(&self.listeners);
        Subscription::new(move || {
            if let Ok(mut listeners) = listeners.lock() {
                listeners.retain(|(current, _)| *current != id);
            }
        })
    }
}

impl Registry for RegistryImpl {
    fn install(&self, extension: Extension) -> Result<(), RegistryError> {
        validate_extension(&extension)?;
        let installed = self.snapshot().installed();
        self.publish(installed_with(extension, installed))
    }

    fn uninstall(&self, extension: &Extension) {
        let installed = self.snapshot().installed();
        if !installed.iter().any(|item| item.name == extension.name) {
            return;
        }
        let next: Vec<Arc<Extension>> = installed
            .into_iter()
            .filter(|item| item.name != extension.name)
            .collect();
        let _ = self.publish(next);
    }
}

fn installed_with(extension: Extension, mut installed: Vec<Arc<Extension>>) -> Vec<Arc<Extension>> {
    let wrapped = Arc::new(extension);
    match installed.iter().position(|item| item.name == wrapped.name) {
        Some(index) => installed[index] = wrapped,
        None => installed.push(wrapped),
    }
    installed
}

/// The three real built-in task definitions (`pi.generation`, `pi.tool`,
/// `pi.compaction`).
pub fn builtin_tasks() -> Vec<AnyTask> {
    vec![
        Arc::new(crate::harness::generation::GenerationTask) as AnyTask,
        Arc::new(crate::harness::tool::ToolTask),
        Arc::new(crate::harness::compaction::CompactionTask),
    ]
}

/// Create an application-owned registry holding the built-in task definitions.
pub fn create_registry() -> RegistryImpl {
    RegistryImpl::with_builtins(builtin_tasks())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::json::JsonValue;
    use crate::harness::types::TaskDefinition;

    struct FakeTask(&'static str);

    impl TaskDefinition for FakeTask {
        fn name(&self) -> &str {
            self.0
        }
        fn version(&self) -> u64 {
            1
        }
        fn initial(&self) -> JsonValue {
            JsonValue::Null
        }
    }

    fn extension(name: &str) -> Extension {
        Extension {
            name: name.to_string(),
            ..Extension::default()
        }
    }

    fn names(registry: &RegistryImpl) -> Vec<String> {
        registry
            .snapshot()
            .installed()
            .iter()
            .map(|extension| extension.name.clone())
            .collect()
    }

    #[test]
    fn install_replaces_by_name_and_uninstall_removes() {
        let registry = create_registry();
        registry.install(extension("a")).unwrap();
        registry.install(extension("b")).unwrap();
        assert_eq!(names(&registry), vec!["a", "b"]);
        registry.install(extension("a")).unwrap();
        assert_eq!(names(&registry), vec!["a", "b"]);
        registry.uninstall(&extension("a"));
        assert_eq!(names(&registry), vec!["b"]);
    }

    #[test]
    fn subscribe_runs_after_publication_and_stops_on_unsubscribe() {
        let registry = RegistryImpl::with_builtins(Vec::new());
        let seen = Arc::new(Mutex::new(0usize));
        let captured = Arc::clone(&seen);
        let subscription = registry.subscribe(Arc::new(move || {
            *captured.lock().unwrap() += 1;
        }));
        registry.install(extension("a")).unwrap();
        assert_eq!(*seen.lock().unwrap(), 1);
        subscription.unsubscribe();
        registry.install(extension("b")).unwrap();
        assert_eq!(*seen.lock().unwrap(), 1);
    }

    #[test]
    fn validation_rejects_reserved_and_invalid_section_keys() {
        let reserved = Extension {
            name: "bad".to_string(),
            sections: vec![crate::harness::define::section(
                "instructions",
                Arc::new(|_i, _c| Box::pin(async { None })),
                None,
            )],
            ..Extension::default()
        };
        assert!(matches!(
            validate_extension(&reserved),
            Err(RegistryError::ReservedSectionKey { .. })
        ));
        let invalid = Extension {
            name: "bad".to_string(),
            sections: vec![crate::harness::define::section(
                "Bad Key",
                Arc::new(|_i, _c| Box::pin(async { None })),
                None,
            )],
            ..Extension::default()
        };
        assert!(matches!(
            validate_extension(&invalid),
            Err(RegistryError::InvalidSectionKey { .. })
        ));
    }

    #[test]
    fn builtin_task_names_collide() {
        let builtin: AnyTask = Arc::new(FakeTask(GENERATION_TASK_NAME));
        let registry = RegistryImpl::with_builtins(vec![Arc::clone(&builtin)]);
        let mut extension = extension("ext");
        extension.tasks = vec![Arc::new(FakeTask(GENERATION_TASK_NAME))];
        assert!(matches!(
            registry.install(extension),
            Err(RegistryError::DuplicateTaskName { .. })
        ));
        assert!(registry.snapshot().task(GENERATION_TASK_NAME).is_some());
    }
}
