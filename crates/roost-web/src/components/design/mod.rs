//! The `/design` gallery: every colour role, the type, spacing, radius and
//! elevation scales, and every `components/ui` primitive on one page, the visual
//! reference new surfaces are matched against. `app.rs` routes `/design` to
//! [`DesignGallery`]. The specimens use local state over shipped `ui` primitives
//! and Tailwind theme roles; none of them reads the store.

pub mod catalog;
pub mod color_roles;
mod composer_specimen;
pub mod content_primitives;
pub mod control_states;
pub mod gallery;
pub mod loader_specimen;
pub mod overlay_states;
pub mod palette_family;
pub mod settings_navigation_specimen;
pub mod token_sections;
pub mod workbench_shell_specimen;

pub use gallery::DesignGallery;
