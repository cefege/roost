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
pub mod agents;
pub mod app_error_boundary;
pub mod brand_mark;
pub mod browse;
pub mod context_menu;
pub mod deck;
pub mod design;
pub mod file_viewer;
pub mod global_search;
pub mod help;
pub mod home;
pub mod layout;
pub mod machines;
pub mod main_pane;
pub mod md;
pub mod mobile_voice_dom;
pub mod mobile_voice_input;
pub mod mobile_voice_shell;
pub mod mobile_voice_watchdog;
pub mod not_served;
pub mod notifications;
pub mod pairing;
pub mod palette;
pub mod rename_dialog;
pub mod settings;
pub mod settings_navigation;
pub mod sidebar;
pub mod term_font_stepper;
pub mod terminal;
pub mod terminal_chrome;
