//! The primary folders sidebar: browser entry, filter, rows and adapters.
//! AppShell mounts SidebarRoot on desktop and in the compact drawer.
//! State and derivations live in roost_client_core::store::sidebar.

pub mod all_view;
pub mod context_menu_frame;
#[cfg(target_arch = "wasm32")]
mod dom;
pub mod folder_list;
pub mod folder_row;
pub mod folder_row_context_menu;
pub mod machine_action_items;
pub mod rel_time_tick;
pub mod row_chips;
pub mod row_swipe;
pub mod session_row;
pub mod session_row_context_menu;
pub mod sidebar_empty_state;
pub mod sidebar_heading;
pub mod sidebar_root;
pub mod sidebar_search;
