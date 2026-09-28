//! The sidebar's state and derivations: the keyboard cursor, the browser-local
//! visit memory, the folder buckets, the Agents projection, the viewer avatars,
//! and the one action event that changes any of it. The framework-free half of
//! `apps/web/src/components/sidebar/*` and the `apps/web/src/lib/` modules they
//! import; `roost-web`'s `components::sidebar` renders it.

pub mod agents_projection;
pub mod cursor;
pub mod documents;
pub mod folder_groups;
pub mod format;
pub mod intent;
pub mod memory;
pub mod viewers;

use crate::platform::KeyValueStore;

pub use cursor::SidebarCursor;
pub use intent::{SidebarIntent, apply_sidebar_intent};
pub use memory::SidebarMemory;

/// Everything the sidebar holds between renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarState {
    /// The keyboard cursor over the Folders panel's rows.
    pub cursor: SidebarCursor,
    /// The persisted visit memory.
    pub memory: SidebarMemory,
}

impl SidebarState {
    /// The state a document boots with: an empty cursor and the stored memory.
    pub fn load(storage: &dyn KeyValueStore) -> Self {
        Self {
            cursor: SidebarCursor::default(),
            memory: SidebarMemory::load(storage),
        }
    }
}
