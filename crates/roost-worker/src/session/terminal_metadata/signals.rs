//! One channel's program-to-operator signals on the semantic metadata lane:
//! the OSC 9;4 progress report, OSC 7501 program status and OSC 1337 user
//! variables, which are retained facts re-sent on replay like the title, and
//! OSC 9 / 777 notifications, which are one-shot events like the bell.
//! `TerminalMetadataStage` owns one per channel and folds its pending values
//! into each flushed `TerminalMetadata`.

use roost_protocol::program_status::{
    ProgramStatusRecord, ProgramStatusTable, ProgramStatusUpdate,
};
use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};

/// A channel notifies at most once per this window: a script that notifies in
/// a loop must not flood every browser and phone.
pub const TERMINAL_NOTIFICATION_RATE_LIMIT_MS: i64 = 1_000;

/// What one live chunk's core parse produced.
#[derive(Debug, Default)]
pub struct LiveSignals {
    pub progress: Option<TerminalProgress>,
    pub notifications: Vec<TerminalNotification>,
    /// The whole current set, when it changed.
    pub user_vars: Option<Vec<TerminalUserVar>>,
    /// OSC 7501 updates in the order the core parsed them, ended-program
    /// markers included, so the table sees the same sequence the program sent.
    pub program_status: Vec<ProgramStatusUpdate>,
}

impl LiveSignals {
    pub fn is_empty(&self) -> bool {
        self.progress.is_none()
            && self.notifications.is_empty()
            && self.user_vars.is_none()
            && self.program_status.is_empty()
    }
}

/// The values one flush sends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingSignals {
    pub progress: Option<TerminalProgress>,
    pub notification: Option<TerminalNotification>,
    pub user_vars: Option<Vec<TerminalUserVar>>,
    pub program_status: Option<Vec<ProgramStatusRecord>>,
}

#[derive(Debug, Default, Clone)]
pub struct ChannelSignals {
    /// The latest report; `Clear` is kept until sent, then forgotten.
    progress: Option<TerminalProgress>,
    progress_dirty: bool,
    user_vars: Vec<TerminalUserVar>,
    user_vars_dirty: bool,
    program_status: ProgramStatusTable,
    program_status_dirty: bool,
    notification: Option<TerminalNotification>,
    last_notification_at_ms: Option<i64>,
}

impl ChannelSignals {
    /// Record a live parse. Returns whether anything became owed.
    pub fn observe(&mut self, live: LiveSignals, now_ms: i64) -> bool {
        let mut owed = false;
        if let Some(progress) = live.progress
            && self.progress != Some(progress)
        {
            self.progress = Some(progress);
            self.progress_dirty = true;
            owed = true;
        }
        if let Some(vars) = live.user_vars
            && vars != self.user_vars
        {
            self.user_vars = vars;
            self.user_vars_dirty = true;
            owed = true;
        }
        for update in live.program_status {
            if self.program_status.apply(update) {
                self.program_status_dirty = true;
                owed = true;
                tracing::debug!(
                    records = self.program_status.records().len(),
                    "a program status record changed"
                );
            }
        }
        for notification in live.notifications {
            let due = self.last_notification_at_ms.is_none_or(|last| {
                now_ms.saturating_sub(last) >= TERMINAL_NOTIFICATION_RATE_LIMIT_MS
            });
            if !due {
                tracing::debug!("a terminal notification was dropped by the rate limit");
                continue;
            }
            self.notification = Some(notification);
            self.last_notification_at_ms = Some(now_ms);
            owed = true;
        }
        owed
    }

    /// Re-send the retained facts after a reconnect or negotiation.
    pub fn reassert(&mut self) {
        self.progress_dirty |= self.progress.is_some();
        self.user_vars_dirty |= !self.user_vars.is_empty();
        self.program_status_dirty |= !self.program_status.is_empty();
    }

    /// The channel's process has exited. Progress and user variables describe
    /// the live program, and the coordinator clears them on the close, so
    /// nothing of them is queued. The program status the exit leaves standing
    /// (`done`, `error`) is queued even when nothing was observed since the
    /// last send: a `done` still pending at close must reach the coordinator.
    pub fn close(&mut self) {
        self.progress = None;
        self.progress_dirty = false;
        self.user_vars.clear();
        self.user_vars_dirty = false;
        let surviving = self.program_status.retained_past_exit();
        if surviving.records() != self.program_status.records() {
            self.program_status = surviving;
            self.program_status_dirty = true;
        }
    }

    pub fn dirty(&self) -> bool {
        self.progress_dirty
            || self.user_vars_dirty
            || self.program_status_dirty
            || self.notification.is_some()
    }

    pub fn pending(&self) -> PendingSignals {
        PendingSignals {
            progress: self.progress.filter(|_| self.progress_dirty),
            notification: self.notification.clone(),
            user_vars: self.user_vars_dirty.then(|| self.user_vars.clone()),
            program_status: self
                .program_status_dirty
                .then(|| self.program_status.records()),
        }
    }

    /// The flush delivered `sent`; clear what is still the same.
    pub fn sent(&mut self, sent: &PendingSignals) {
        if sent.progress.is_some() && sent.progress == self.progress {
            self.progress_dirty = false;
            if self.progress == Some(TerminalProgress::Clear) {
                self.progress = None;
            }
        }
        if sent.user_vars.as_ref() == Some(&self.user_vars) {
            self.user_vars_dirty = false;
        }
        if sent.program_status.as_ref() == Some(&self.program_status.records()) {
            self.program_status_dirty = false;
        }
        if sent.notification.is_some() && sent.notification == self.notification {
            self.notification = None;
        }
    }
}
