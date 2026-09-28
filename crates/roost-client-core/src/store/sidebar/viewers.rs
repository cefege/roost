//! The per-session viewer avatars: one per browser tab viewing a session, its
//! monogram and hue, and whether it holds the binding minimum the PTY is sized
//! to. The logic half of `apps/web/src/components/sidebar/ViewersChip.tsx`;
//! the `ViewersChip` component renders it.

use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry, minimum_terminal_geometry};

use crate::store::Store;
use crate::store::sidebar::format::{FpColor, color_for_fp};

/// One avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerAvatar {
    /// The viewer's fingerprint.
    pub fp: String,
    /// Its label, empty when it has none.
    pub label: String,
    /// The label, or the 8-character fingerprint prefix.
    pub name: String,
    /// The first character of `name`, upper-cased.
    pub monogram: String,
    /// Whether this viewer holds the binding width or height.
    pub controlling: bool,
    /// The avatar's tone.
    pub color: FpColor,
    /// The native hover text.
    pub title: String,
}

/// The avatars for one session, in the coordinator's order.
///
/// A controller is flagged only under contention (two or more viewers): a sole
/// viewer trivially controls and there is nothing to disambiguate. Viewers
/// without a live geometry claim (a legacy fps-only frame projects 0×0) never
/// bind, and the minimum is `minimum_terminal_geometry` — the one the
/// coordinator sizes the PTY with — so the halo and the PTY cannot disagree.
pub fn session_viewer_avatars(store: &Store, session_id: &str) -> Vec<ViewerAvatar> {
    let Some(entries) = store.session_viewers.get(session_id) else {
        return Vec::new();
    };
    let geometry = |cols: u32, rows: u32| TerminalGeometry { cols, rows };
    let claims: Vec<TerminalGeometry> = entries
        .iter()
        .map(|entry| geometry(entry.cols, entry.rows))
        .filter(is_terminal_geometry)
        .collect();
    let binding = minimum_terminal_geometry(claims.iter()).ok().flatten();
    entries
        .iter()
        .map(|entry| {
            let claim = geometry(entry.cols, entry.rows);
            let controlling = binding.is_some_and(|binding| {
                entries.len() > 1
                    && is_terminal_geometry(&claim)
                    && (claim.cols == binding.cols || claim.rows == binding.rows)
            });
            let label = entry.label.clone().unwrap_or_default();
            let name = viewer_display_name(&entry.fp, &label);
            let fp_prefix: String = entry.fp.chars().take(8).collect();
            let identity = if label.is_empty() {
                format!("viewer {fp_prefix}")
            } else {
                format!("{label} ({fp_prefix})")
            };
            ViewerAvatar {
                fp: entry.fp.clone(),
                monogram: name.chars().take(1).flat_map(char::to_uppercase).collect(),
                name,
                controlling,
                color: color_for_fp(&entry.fp),
                title: if controlling {
                    format!("{identity} — controls terminal size")
                } else {
                    identity
                },
                label,
            }
        })
        .collect()
}

/// A non-blank label, else the fingerprint before any `:` suffix, cut to 8.
pub fn viewer_display_name(fp: &str, label: &str) -> String {
    if !label.trim().is_empty() {
        return label.to_owned();
    }
    let base = fp.split_once(':').map_or(fp, |(head, _)| head);
    base.chars().take(8).collect()
}
