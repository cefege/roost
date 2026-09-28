//! What the agent detector reads from the worker's sessions: the live set with
//! each child pid, a session's visible grid as text with its OSC evidence, and
//! the evidence clear on an agent change. Ports v2 `detector.ts`
//! `readVisibleScreen` and the `SessionManager` reads `AgentScreenDetector`
//! makes. `agents::detector` reads it; `runtime::owners` builds
//! [`TableAgentSessions`] over the session table.

use std::sync::Arc;

use roost_protocol::cell::spans_text;
use roost_protocol::wire::brand::SessionId;
use roost_term::TerminalCore;
use roost_term::frame::viewport_row_spans;

use crate::session::table::SessionTable;

/// One live session as the detector sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionView {
    pub session_id: SessionId,
    pub channel_id: u16,
    pub child_pid: Option<u32>,
}

/// The manifest inputs one session offers: its visible grid as text and the
/// OSC title and progress its byte stream last carried ("" when none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenEvidence {
    pub screen: String,
    pub osc_title: String,
    pub osc_progress: String,
}

/// The session reads a detector makes (v2 `SessionManager.allSessions`,
/// `getBySessionId`, the record's core and raw OSC fields).
pub trait AgentSessionSource: Send + Sync {
    fn all_sessions(&self) -> Vec<AgentSessionView>;
    fn session(&self, session_id: &SessionId) -> Option<AgentSessionView>;
    /// The expensive read: every visible cell of the session's grid.
    fn screen_evidence(&self, session_id: &SessionId) -> Option<ScreenEvidence>;
    /// v2 `clearAgentOscEvidence`: forget the retained title and progress.
    fn clear_osc_evidence(&self, session_id: &SessionId);
}

/// v2 `readVisibleScreen`: the visible grid as text for manifest matching.
/// Rows come from the cell encoder, not a private per-column read: a wide
/// glyph's width-0 continuation cell must contribute NOTHING, or every pattern
/// that spans one sees a phantom space ("中 文") and stops matching.
pub fn read_visible_screen(core: &dyn TerminalCore) -> String {
    let cols = core.cols();
    let rows = core.rows();
    let lines: Vec<String> = (0..rows)
        .map(|row| {
            spans_text(&viewport_row_spans(core, row, cols))
                .trim_end()
                .to_owned()
        })
        .collect();
    lines.join("\n")
}

/// [`AgentSessionSource`] over the worker's session table.
#[derive(Debug, Clone)]
pub struct TableAgentSessions {
    table: Arc<SessionTable>,
}

impl TableAgentSessions {
    pub fn new(table: Arc<SessionTable>) -> Self {
        Self { table }
    }
}

impl AgentSessionSource for TableAgentSessions {
    fn all_sessions(&self) -> Vec<AgentSessionView> {
        self.table
            .live()
            .into_iter()
            .filter_map(|(session_id, channel_id)| {
                let child_pid = self
                    .table
                    .with_channel_record(channel_id, |record| record.child_pid)?;
                Some(AgentSessionView {
                    session_id,
                    channel_id,
                    child_pid,
                })
            })
            .collect()
    }

    fn session(&self, session_id: &SessionId) -> Option<AgentSessionView> {
        let channel_id = self.table.channel_of(session_id)?;
        let child_pid = self
            .table
            .with_channel_record(channel_id, |record| record.child_pid)?;
        Some(AgentSessionView {
            session_id: session_id.clone(),
            channel_id,
            child_pid,
        })
    }

    fn screen_evidence(&self, session_id: &SessionId) -> Option<ScreenEvidence> {
        self.table.with_record(session_id, |record| ScreenEvidence {
            screen: read_visible_screen(&*record.terminal_core),
            osc_title: record.agent_osc.raw_title.clone(),
            osc_progress: record.agent_osc.raw_progress.clone(),
        })
    }

    fn clear_osc_evidence(&self, session_id: &SessionId) {
        self.table
            .with_record_mut(session_id, |record| record.agent_osc.clear_evidence());
    }
}
