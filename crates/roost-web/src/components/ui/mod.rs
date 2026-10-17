//! The rust-ui component layer (rust-ui.com, `github.com/rust-ui/ui`): every
//! surface composes its controls, panels, overlays and glyphs from here.
//! Each file is the rust-ui registry component of the same name, adapted to
//! this crate's rules; styling is Tailwind utilities over the theme bridge in
//! `crates/roost-web/tailwind.css`.

pub mod icon;

pub use icon::Icon;
pub use icons::IconType;
