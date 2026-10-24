//! The `/design` gallery uses pinned Libero components directly, alongside
//! Roost's compact workbench and settings-navigation roles.

mod combobox_specimen;
pub mod content_primitives;
pub mod control_states;
mod dialog_specimen;
pub mod gallery;
pub mod overlay_states;
pub mod popover_specimen;
mod theme_roles;

pub use gallery::DesignGallery;
