//! The primary folders sidebar: its filter, pinned new-terminal bar, rows,
//! chips and adapters. Ports `apps/web/src/components/sidebar/`; `AppShell`
//! mounts `SidebarRoot`; its state and derivations live in
//! `roost_client_core::store::sidebar`.

pub mod agent_conversation_row;
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
pub mod session_row_flat;
pub mod sidebar_empty_state;
pub mod sidebar_new_terminal;
pub mod sidebar_root;
pub mod sidebar_search;
pub mod viewers_chip;
