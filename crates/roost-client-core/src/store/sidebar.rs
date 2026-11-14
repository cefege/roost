//! The sidebar's state and derivations: keyboard cursor, visit memory,
//! folder buckets, viewer avatars, and actions. The web client renders these
//! framework-free projections.

pub mod cursor;
pub mod documents;
pub mod folder_groups;
pub mod format;
pub mod intent;
pub mod memory;
pub mod viewers;

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
