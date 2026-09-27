//! The command palette's catalog: session rows, workspace rows, and the closed
//! set of core actions, compiled from the same navigation index the search page
//! reads.
//!
//! One builder, and the rows it produces are PLAIN DATA with a stable id. v2 kept
//! an identity cache whose whole job was to hand `<For>` back the previous object
//! for an unchanged row (`command-palette-data.ts:239-254`); a row keyed by a
//! stable id gets that for free, and a cache of items holding closures cannot
//! survive a credential boundary without being cleared by name. So the cache is
//! gone and its invariant is carried by [`PaletteItem::id`].
//!
//! The identity that matters is the one that changes per CREDENTIAL: a contextual
//! action captures a target and a generation, so its id folds both
//! (`command-palette-data.ts:227-233`). After a sign-out the same folder produces
//! a DIFFERENT row, which is what stops a stale closure from being pressed. The
//! generation check itself is
//! [`crate::store::root::captured_generation_is_current`] — one predicate on the
//! store rather than the two inline comparisons v2 makes at
//! `command-palette-data.ts:196,217`.
//!
//! Ported from `apps/web/src/store/command-palette-data.ts`. The two identical
//! target interfaces (`CommandPaletteSessionTarget` and
//! `CommandPaletteFolderTarget`) are one [`PaletteTarget`] here: v2 declares the
//! same three fields twice.

use crate::store::navigation::{NavigationSearchDocument, query};

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A session.
    Session,
    /// A workspace.
    Workspace,
    /// A command.
    Action,
}

impl ItemKind {
    /// The wire spelling, and the prefix of a row's id.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Action => "action",
        }
    }
}

/// What a command row does, as data.
///
/// Not a closure: a store that held a callback could not be `Debug`, and the
/// closure would capture a target the user may no longer be allowed to act on.
/// Each variant names the host's own action, and the host is the only thing that
/// can perform one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Open the task dialog for a folder.
    QueueFolderTask {
        /// The machine the task runs on.
        worker_fp: String,
        /// The folder it runs in.
        cwd: String,
    },
    /// Spawn a terminal beside this one, in the same folder.
    SpawnSibling {
        /// The machine to spawn on.
        worker_fp: String,
        /// The folder to spawn in.
        cwd: String,
    },
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// The row's stable identity. The host keys its rendered rows on this, and
    /// an action's id folds the credential generation, so a row from a
    /// superseded credential is a DIFFERENT row rather than a stale one.
    pub id: String,
    /// What the row is.
    pub kind: ItemKind,
    /// What the row shows.
    pub label: String,
    /// The dimmer text beside it.
    pub hint: Option<String>,
    /// Extra text folded into the match but not displayed.
    pub search: Option<String>,
    /// Where the row navigates to, when it navigates.
    pub href: Option<String>,
    /// What the row does, when it is a command.
    pub action: Option<PaletteAction>,
    /// The credential generation this row captured, for an action row.
    pub captured_auth_generation: Option<u64>,
}

impl PaletteItem {
    /// The id a session row carries.
    pub fn session_id(session_id: &str) -> String {
        format!("{}:{session_id}", ItemKind::Session.as_str())
    }

    /// The id a workspace row carries.
    pub fn workspace_id(workspace_id: &str) -> String {
        format!("{}:{workspace_id}", ItemKind::Workspace.as_str())
    }

    /// The id a contextual action carries, with the credential generation folded
    /// in.
    pub fn targeted_action_id(action_id: &str, target_id: &str, auth_generation: u64) -> String {
        format!("{action_id}:{target_id}:generation:{auth_generation}")
    }
}

/// The thing a contextual action acts on.
///
/// One type, because v2 declared the same three fields twice: a "session target"
/// and a "folder target" are the same scalar triple, and the difference between
/// them was which closure read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteTarget {
    /// The session, or the folder, this target names.
    pub id: String,
    /// The machine it is on.
    pub worker_fp: String,
    /// The folder it is in.
    pub cwd: String,
}

/// What the palette knows about where the user is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandPaletteContext {
    /// The credential generation every action in this build captures.
    pub auth_generation: u64,
    /// The session the current route resolved to, if any.
    pub active_session: Option<PaletteTarget>,
    /// The folder the current route resolved to, if any.
    pub active_folder: Option<PaletteTarget>,
    /// Whether the active machine can be reached now. A sibling spawn against an
    /// unreachable machine fails, so the row is not offered.
    pub worker_routable: bool,
}

/// The catalog's closed set of core action ids.
pub const CORE_ACTION_IDS: [&str; 4] = [
    "core.search.all",
    "core.attention.open",
    "core.task.queue-folder",
    "core.session.new-sibling",
];

/// Whether `text` matches every one of the already-normalized terms.
///
/// The caller normalizes and splits ONCE per filter run; matching re-normalizes
/// the candidate and checks every term against it.
pub fn matches_query(text: &str, normalized_query_terms: &[String]) -> bool {
    if normalized_query_terms.is_empty() {
        return true;
    }
    let normalized = query::normalize_navigation_search_query(text);
    normalized_query_terms
        .iter()
        .all(|term| normalized.contains(term.as_str()))
}

/// Compile the current session rows, the workspace rows, and whichever core
/// actions this context has a target for.
///
/// `documents` is the navigation index, so a palette row and a search row are the
/// same row and cannot describe a session differently.
pub fn build_default_items(
    context: &CommandPaletteContext,
    documents: &[NavigationSearchDocument],
    workspaces: &[roost_protocol::wire::Workspace],
) -> Vec<PaletteItem> {
    let mut items: Vec<PaletteItem> = documents
        .iter()
        .map(|document| PaletteItem {
            id: PaletteItem::session_id(&document.session_id),
            kind: ItemKind::Session,
            label: document.display_title.clone(),
            hint: Some(if document.available {
                document.worker_label.clone()
            } else {
                format!("{} · unavailable", document.worker_label)
            }),
            search: Some(document.search_text.clone()),
            href: Some(document.href.clone()),
            action: None,
            captured_auth_generation: None,
        })
        .collect();
    items.extend(workspaces.iter().map(|workspace| PaletteItem {
        id: PaletteItem::workspace_id(workspace.id.as_str()),
        kind: ItemKind::Workspace,
        label: workspace.name.clone(),
        hint: Some(plural_sessions(workspace.session_ids.len())),
        href: Some(format!("/w/{}", workspace.id)),
        search: Some(workspace.folder_path.clone()),
        action: None,
        captured_auth_generation: None,
    }));
    items.extend(core_action_items(context));
    items
}

/// The core rows, in the order the closed set names them.
///
/// Two are always present — they navigate, and there is nothing to be contextual
/// about. The other two are compiled only when there is a target, which is what
/// `compileQueueFolder` and `compileNewSibling` return `null` for.
pub fn core_action_items(context: &CommandPaletteContext) -> Vec<PaletteItem> {
    let mut items = vec![
        PaletteItem {
            id: "core.search.all".to_owned(),
            kind: ItemKind::Action,
            label: "Search all sessions".to_owned(),
            hint: Some("metadata".to_owned()),
            search: Some("global search sessions workspaces workers git ports".to_owned()),
            href: Some("/search".to_owned()),
            action: None,
            captured_auth_generation: None,
        },
        PaletteItem {
            id: "core.attention.open".to_owned(),
            kind: ItemKind::Action,
            label: "Open attention".to_owned(),
            hint: Some("blocked and completed agents".to_owned()),
            search: Some("attention blocked done unseen agents".to_owned()),
            href: Some("/search?scope=attention".to_owned()),
            action: None,
            captured_auth_generation: None,
        },
    ];
    if let Some(target) = context.active_folder.clone() {
        items.push(targeted_action(
            "core.task.queue-folder",
            &target,
            context.auth_generation,
            "Queue task for this folder",
            PaletteAction::QueueFolderTask {
                worker_fp: target.worker_fp.clone(),
                cwd: target.cwd.clone(),
            },
        ));
    }
    if let (Some(target), true) = (context.active_session.clone(), context.worker_routable) {
        items.push(targeted_action(
            "core.session.new-sibling",
            &target,
            context.auth_generation,
            "New sibling terminal",
            PaletteAction::SpawnSibling {
                worker_fp: target.worker_fp.clone(),
                cwd: target.cwd.clone(),
            },
        ));
    }
    items
}

fn targeted_action(
    action_id: &str,
    target: &PaletteTarget,
    auth_generation: u64,
    label: &str,
    action: PaletteAction,
) -> PaletteItem {
    PaletteItem {
        id: PaletteItem::targeted_action_id(action_id, &target.id, auth_generation),
        kind: ItemKind::Action,
        label: label.to_owned(),
        hint: Some(target.cwd.clone()),
        search: Some(format!("{label} {}", target.cwd)),
        href: None,
        action: Some(action),
        captured_auth_generation: Some(auth_generation),
    }
}

/// Whether a compiled row may still be pressed.
///
/// The row's captured generation against the store's, through
/// [`crate::store::root::captured_generation_is_current`] — the ONE predicate,
/// which takes the store. A host calls it with
/// `item.captured_auth_generation` before performing `item.action`, which is the
/// check v2 makes inline at two call sites
/// (`command-palette-data.ts:196,217`). A row that captures nothing returns
/// `None` here and needs no check: it holds no credential, so no boundary can have
/// invalidated it.
pub fn captured_generation(item: &PaletteItem) -> Option<u64> {
    item.captured_auth_generation
}

fn plural_sessions(count: usize) -> String {
    if count == 1 {
        "1 session".to_owned()
    } else {
        format!("{count} sessions")
    }
}
