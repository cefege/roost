//! The sidebar's keyboard cursor: the row order the Folders panel publishes
//! and the highlighted index, so the global ↑/↓/⏎ handler and the rendered
//! rows agree on one row. Ports `apps/web/src/lib/sidebarCursor.ts`. FolderList
//! publishes; the keyboard shortcuts read and move it.
//!
//! The cursor is NOT "selected": selected is the URL match (`data-selected`),
//! the cursor is the keyboard highlight (`data-cursor`).

/// The published row order and the highlight over it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarCursor {
    ids: Vec<String>,
    /// `None` = no row highlighted (v2's `-1`).
    index: Option<usize>,
}

impl SidebarCursor {
    /// Publish the rendered row order, clamping the highlight into range.
    ///
    /// An unchanged order is a no-op, so a rebuild that re-publishes the same
    /// rows does not repaint every row's `data-cursor`.
    pub fn set_ordered_session_ids(&mut self, ids: Vec<String>) -> bool {
        if self.ids == ids {
            return false;
        }
        self.index = match (self.index, ids.len()) {
            (_, 0) => None,
            (Some(index), len) if index >= len => Some(len - 1),
            (index, _) => index,
        };
        self.ids = ids;
        true
    }

    /// The highlighted session, if any.
    pub fn cursor_session_id(&self) -> Option<&str> {
        self.index
            .and_then(|index| self.ids.get(index))
            .map(String::as_str)
    }

    /// Whether there are rows the arrows can move over. Off the sidebar the
    /// order is empty, and claiming the keys there would cancel native scroll.
    pub fn has_cursor_targets(&self) -> bool {
        !self.ids.is_empty()
    }

    /// Move by `delta`, clamped to the rows. From no highlight, ↓ lands on the
    /// first row and ↑ on the last; there is no wrap.
    pub fn move_cursor(&mut self, delta: i32) -> bool {
        let before = self.index;
        let len = self.ids.len();
        self.index = if len == 0 {
            None
        } else {
            match self.index {
                None if delta > 0 => Some(0),
                None => Some(len - 1),
                Some(current) => {
                    let moved = i64::try_from(current)
                        .unwrap_or(i64::MAX)
                        .saturating_add(i64::from(delta));
                    let last = i64::try_from(len - 1).unwrap_or(i64::MAX);
                    usize::try_from(moved.clamp(0, last)).ok()
                }
            }
        };
        before != self.index
    }
}
