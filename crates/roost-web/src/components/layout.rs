//! The workbench chrome: the grid every path is drawn inside, and the six pieces
//! that fill it. Ported from `apps/web/src/components/layout/AppShell.tsx` and
//! the `Workbench*` components beside it.
//!
//! The chrome is STRUCTURAL. It owns the grid, the sidebar region's width, the
//! compact/desktop split and the status readouts; it does not own a session list,
//! a terminal, or a settings pane, and nothing here reaches for one. The route
//! content arrives as this module's `children`, in the editor slot.
//!
//! `shell_metrics` holds every decision; the components here turn a decision into
//! class names and attributes that `assets/styles/workbench-shell.css` already
//! draws. No inline colour, no raw size, and no geometry decided in a component
//! that a test cannot reach.

pub mod activity_bar;
pub mod app_shell;
pub mod mobile_bar;
pub mod shell_metrics;
pub mod sidebar_region;
pub mod status_bar;
pub mod title_bar;

pub use app_shell::AppShell;
