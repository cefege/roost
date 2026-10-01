//! The shell's UI bridge: this tab's report to the coordinator, and the
//! coordinator's commands to this tab.
//!
//! Ports `apps/web/src/components/UiBridge.tsx`, which v2 mounted once inside
//! the router so a coordinator command shared the reader's own navigation. The
//! two halves are [`report::UiStateReporter`] — what this tab is showing, on the
//! cadence in `client::ui_command` — and [`commands`] — what a drained
//! `UiCommandAction` means as a browser operation.
//!
//! MOUNTED IN THE SHELL, NOT IN A PANE. The report cadence has to outlive every
//! terminal surface: a pane unmounts on navigation, and a tab that stopped
//! reporting because its reader closed a deck would leave the coordinator
//! holding an arrangement nobody is showing. The component renders nothing, and
//! deliberately does NOT subscribe to the store's revision — the sweep is what
//! drives it, and a bridge that repainted on every cell frame would be a second
//! reader of the store for no gain.

pub mod apply;
pub mod commands;
pub mod host;
pub mod report;

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;

pub use apply::run_acknowledged_layout_apply;
pub use commands::drain_ui_commands;
pub use host::{BrowserBridgeHost, ShellFacts, UiBridgeHost};
pub use report::UiStateReporter;

/// The bridge's own state, held for the life of the shell mount.
#[derive(Debug, Default)]
pub struct UiBridgeState {
    shell: ShellFacts,
    reporter: UiStateReporter,
    /// Whether the cadence has been armed yet.
    ///
    /// `UiStateReportCadence::request` is DOCUMENTED as ignored before `start`,
    /// and a bridge that only ever requested armed nothing: every sweep asked
    /// for a report that could not become due, and the tab reported nothing for
    /// the life of the document. The arm belongs here rather than at mount
    /// because the sweep's `now_ms` is the same timeline the cadence reads,
    /// and a mount-time clock would be a second answer to "when".
    started: bool,
}

impl UiBridgeState {
    /// Record what the shell is showing.
    ///
    /// Called during render, which is the only place the router's path and the
    /// window's size class are readable; the sweep then reads a plain value
    /// rather than a signal from outside every Dioxus scope.
    pub fn set_shell(&mut self, path: &str, compact: bool) {
        self.shell = ShellFacts {
            path: path.to_owned(),
            compact,
        };
    }

    /// What the shell last published, for a caller that has to see it.
    pub fn shell(&self) -> &ShellFacts {
        &self.shell
    }

    /// Begin reporting: the tab exists, so one report is owed now.
    pub fn start(&mut self, now_ms: u64) {
        self.reporter.start(now_ms);
    }

    /// Stop reporting; a pending send is dropped.
    pub fn stop(&mut self) {
        self.reporter.stop();
    }

    /// One sweep of the pump's clock: drain what the coordinator queued, then
    /// answer the report cadence.
    pub fn sweep(&mut self, pump: &Pump, host: &mut dyn UiBridgeHost, now_ms: u64) {
        if !self.started {
            self.started = true;
            self.reporter.start(now_ms);
        }
        let shell = self.shell.clone();
        drain_ui_commands(pump, host, &shell);
        let core = pump.core();
        let core = core.borrow();
        self.reporter
            .tick(host, core.store(), &BrowserWorkerPaths, &shell.path, now_ms);
    }
}

/// The bridge, mounted once by the shell. Paints nothing.
#[component]
pub fn UiBridge() -> Element {
    let pump = crate::pump::use_pump();
    let path = crate::router_state::use_location();
    let compact = crate::components::layout::window_size::use_is_compact();
    let state = use_context_provider(|| Rc::new(RefCell::new(UiBridgeState::default())));
    let host =
        use_context_provider(|| Rc::new(RefCell::new(BrowserBridgeHost::new(pump.clone(), path))));
    {
        // The route and the size class are read HERE so this component repaints
        // on a resize; the sweep then finds them already published.
        let rendered = path().to_string();
        state.borrow_mut().set_shell(&rendered, compact);
    }
    let token = use_hook({
        let listener_pump = pump.clone();
        let listener_state = Rc::clone(&state);
        let listener_host = Rc::clone(&host);
        move || {
            let swept = listener_pump.clone();
            listener_pump.on_sweep(Rc::new(move |now_ms| {
                let mut host = listener_host.borrow_mut();
                let mut state = listener_state.borrow_mut();
                state.sweep(&swept, &mut *host, now_ms);
            }))
        }
    });
    let cleanup_pump = pump.clone();
    use_drop(move || cleanup_pump.remove_sweep_listener(token));
    rsx! {}
}
