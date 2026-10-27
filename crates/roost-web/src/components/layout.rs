//! The workbench chrome: the grid every in-shell path is drawn inside, and the
//! pieces that fill it. Ports `apps/web/src/components/layout/*` (AppShell,
//! WorkbenchActivityBar, SidebarResizer, WorkbenchStatusBar, MobileTopBar,
//! MobileSidebarDrawer); the phone-size switch is rust-ui's `use_is_mobile`
//! (`window_size`).
//!
//! The chrome is STRUCTURAL: it owns the grid, the sidebar region's width, the
//! compact/desktop split and the status readouts; the route content arrives as
//! `AppShell`'s children. `shell_metrics` and `shell_style` hold the decisions;
//! the Tailwind classes in `app_shell.rs` draw them.

pub mod activity_bar;
pub mod app_shell;
#[cfg(target_arch = "wasm32")]
mod app_shell_dom;
pub mod drawer_gesture;
pub mod keyboard_offset;
pub mod mobile_bar;
pub mod mobile_sidebar_drawer;
pub mod notification_dock_lift;
pub mod portal;
pub mod shell_metrics;
pub mod shell_style;
pub mod sidebar_region;
pub mod sidebar_resizer;
pub mod status_bar;
pub mod window_size;

pub use app_shell::AppShell;
