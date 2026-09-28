//! Worker-native path navigation for the `/browse` picker: child and parent
//! paths, the breadcrumb trail, and the middle-collapse a narrow header folds
//! it to. Ports `apps/web/src/lib/folderPalette.ts`; every separator and root
//! rule delegates to the parent module's codec. The browse surface calls it.

use super::{WorkerPathCrumb, join_worker_path, worker_path_crumbs, worker_path_dirname};

/// One entry of a collapsed trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrumbView {
    /// A visible crumb.
    Crumb(WorkerPathCrumb),
    /// The folded middle, in trail order.
    Ellipsis(Vec<WorkerPathCrumb>),
}

/// The path of `name` inside `dir`; `None` when `name` is itself absolute.
pub fn child_path(worker_os: Option<&str>, dir: &str, name: &str) -> Option<String> {
    join_worker_path(worker_os, dir, &[name])
}

/// The breadcrumb trail for a browse header; empty for an empty `dir`.
pub fn path_crumbs(worker_os: Option<&str>, dir: &str) -> Vec<WorkerPathCrumb> {
    if dir.is_empty() {
        return Vec::new();
    }
    worker_path_crumbs(worker_os, dir)
}

/// Fold `hide_middle` of the crumbs between the root and parent+current into
/// one ellipsis, hiding from the LEFT so the segments nearest the current
/// folder stay visible longest. Three or fewer crumbs have no middle.
pub fn collapse_crumbs_to(crumbs: &[WorkerPathCrumb], hide_middle: usize) -> Vec<CrumbView> {
    if crumbs.len() <= 3 {
        return crumbs.iter().cloned().map(CrumbView::Crumb).collect();
    }
    let middle = &crumbs[1..crumbs.len() - 2];
    let hidden = hide_middle.min(middle.len());
    let mut out = vec![CrumbView::Crumb(crumbs[0].clone())];
    if hidden > 0 {
        out.push(CrumbView::Ellipsis(middle[..hidden].to_vec()));
    }
    out.extend(crumbs[1 + hidden..].iter().cloned().map(CrumbView::Crumb));
    out
}

/// The Up button's target. An empty path and the browse `~` sentinel stay
/// put; the codec keeps every native root its own parent.
pub fn parent_path(worker_os: Option<&str>, dir: &str) -> String {
    if dir.is_empty() || dir == "~" {
        return dir.to_owned();
    }
    worker_path_dirname(worker_os, dir).unwrap_or_else(|| dir.to_owned())
}
