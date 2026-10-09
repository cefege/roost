//! The progress reports and desktop notifications rio's parse produced and
//! [`super::RioCore`] has not taken. The listener (`listener.rs`) fills it from
//! `RioEvent::ProgressReport` / `TerminalNotification`; a replay `write`
//! discards it, so history never re-notifies.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rio_vt::event::{ProgressReport, ProgressState};

use crate::signals::{
    NOTIFICATION_TEXT_MAX_CHARS, NOTIFICATIONS_PER_PARSE_MAX, TerminalNotification,
    TerminalProgress, TerminalUserVar, bounded_user_vars, truncated,
};

#[derive(Debug, Default)]
struct PendingSignals {
    progress: Option<TerminalProgress>,
    notifications: Vec<TerminalNotification>,
}

/// Shared between the listener and the core, like the other queues.
#[derive(Debug, Default, Clone)]
pub(crate) struct SignalQueue {
    pending: Arc<Mutex<PendingSignals>>,
}

impl SignalQueue {
    pub(crate) fn take_progress(&self) -> Option<TerminalProgress> {
        self.lock().progress.take()
    }

    pub(crate) fn take_notifications(&self) -> Vec<TerminalNotification> {
        std::mem::take(&mut self.lock().notifications)
    }

    pub(crate) fn discard(&self) {
        let mut pending = self.lock();
        pending.progress = None;
        pending.notifications.clear();
    }

    pub(super) fn record_progress(&self, report: ProgressReport) {
        let progress = match report.state {
            ProgressState::Remove => TerminalProgress::Clear,
            ProgressState::Set => TerminalProgress::Normal(report.progress.unwrap_or(0).min(100)),
            ProgressState::Error => TerminalProgress::Error(report.progress.map(|p| p.min(100))),
            ProgressState::Indeterminate => TerminalProgress::Indeterminate,
            ProgressState::Pause => TerminalProgress::Paused(report.progress.map(|p| p.min(100))),
        };
        self.lock().progress = Some(progress);
    }

    pub(super) fn record_notification(&self, title: &str, body: &str) {
        // Rio reads every OSC 9 other than 9;4 and 9;9 as a notification,
        // so ConEmu's numeric sub-commands (9;1 sleep, 9;2 message box, ...)
        // would toast as "1;500"; they are not notifications.
        if title.is_empty() && is_conemu_subcommand(body) {
            return;
        }
        let mut pending = self.lock();
        if pending.notifications.len() >= NOTIFICATIONS_PER_PARSE_MAX {
            return;
        }
        pending.notifications.push(TerminalNotification {
            title: truncated(title, NOTIFICATION_TEXT_MAX_CHARS),
            body: truncated(body, NOTIFICATION_TEXT_MAX_CHARS),
        });
    }

    fn lock(&self) -> MutexGuard<'_, PendingSignals> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `<digits>` alone or `<digits>;…`.
fn is_conemu_subcommand(body: &str) -> bool {
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(body.as_bytes().get(digits), None | Some(b';'))
}

/// The shell's user variables as last published, and whether they changed
/// since the worker last looked. Rio keeps them in a map with no event, so
/// the core compares after each parse.
#[derive(Debug, Default)]
pub(crate) struct UserVarsWatch {
    current: Vec<TerminalUserVar>,
    changed: bool,
}

impl UserVarsWatch {
    pub(crate) fn observe<S>(&mut self, vars: &std::collections::HashMap<String, String, S>) {
        if vars.is_empty() && self.current.is_empty() {
            return;
        }
        let bounded = bounded_user_vars(vars.iter());
        if bounded != self.current {
            self.current = bounded;
            self.changed = true;
        }
    }

    pub(crate) fn current(&self) -> Vec<TerminalUserVar> {
        self.current.clone()
    }

    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}
