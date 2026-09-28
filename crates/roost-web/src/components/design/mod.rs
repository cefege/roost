//! The `/design` gallery: every token and every md primitive on one page, the
//! visual reference new surfaces are matched against. Ported from
//! `apps/web/src/components/design/*`; `app.rs` routes `/design` to
//! [`DesignGallery`]. The specimens here are static markup over the shipped
//! primitives in `components/md`; none of them reads the store.

pub mod catalog;
pub mod content_primitives;
pub mod control_states;
pub mod gallery;
pub mod overlay_states;
pub mod settings_navigation_specimen;
pub mod token_sections;
pub mod workbench_shell_specimen;

pub use gallery::DesignGallery;
