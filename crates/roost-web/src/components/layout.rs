//! The workbench chrome: the grid every in-shell path is drawn inside, and the
//! pieces that fill it. Ports `apps/web/src/components/layout/*` (AppShell,
//! WorkbenchTitleBar, WorkbenchActivityBar, SidebarResizer, WorkbenchStatusBar,
//! MobileTopBar, MobileSidebarDrawer) and the reactive half of
//! `apps/web/src/browser/windowSizeClass.ts`.
//!
//! The chrome is STRUCTURAL: it owns the grid, the sidebar region's width, the
//! compact/desktop split and the status readouts; the route content arrives as
//! `AppShell`'s children. `shell_metrics` and `shell_style` hold the decisions;
//! `assets/styles/workbench-shell.css` draws them.

pub mod activity_bar;
pub mod app_shell;
#[cfg(target_arch = "wasm32")]
mod app_shell_dom;
pub mod drawer_gesture;
pub mod mobile_bar;
pub mod mobile_sidebar_drawer;
pub mod notification_dock_lift;
pub mod shell_metrics;
pub mod shell_style;
pub mod sidebar_region;
pub mod sidebar_resizer;
pub mod status_bar;
pub mod title_bar;
pub mod window_size;

pub use app_shell::AppShell;
