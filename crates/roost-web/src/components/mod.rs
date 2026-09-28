//! The application surface: the design-system primitives the chrome shares, the
//! access gate, the workbench chrome, and the route content the chrome's editor
//! slot hosts.
//!
//! A component here may read the store and call `handle`, and nothing else — a
//! component that reached a socket, a fetch or a DOM node directly would be a
//! rule `roost-client-core` cannot see. The split is `layout` for chrome that
//! exists on every path, `access_gate` for the screen that exists before the
//! credential is known, and one module per surface.
//!
//! `app::surface_for` is what decides which of these renders. A surface added
//! here without an arm there is unreachable, and a route added there without a
//! module here is a compile error.

pub mod access_gate;
pub mod brand_mark;
pub mod design;
pub mod home;
pub mod layout;
pub mod md;
pub mod not_served;
pub mod settings_navigation;
