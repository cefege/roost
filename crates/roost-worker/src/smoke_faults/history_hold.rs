//! The one direct history response the smoke harness may hold: armed for a
//! session by a command, captured by the next peer scrollback read of that
//! session after its read and before its re-authorization, then released or
//! dropped by a later command. Read by `crate::local_terminal`'s scrollback
//! path; driven by `super::commands`. Ports the history half of v2
//! `TerminalPeerTestFaultState` (`terminal-peer-test-faults.ts:120-150`).

use std::sync::Mutex;

use tokio::sync::oneshot;

use super::lock_state as lock;
use crate::session::ids::mint_uuid;

/// Where the one hold stands.
#[derive(Debug, Default)]
enum HoldStage {
    #[default]
    Idle,
    /// Armed, not yet met by a read; a decision may already have arrived.
    Pending {
        session_id: String,
        hold_id: String,
        deliver: Option<bool>,
    },
    /// A read is waiting on the decision.
    Captured {
        hold_id: String,
        decide: oneshot::Sender<bool>,
    },
}

/// v2's pending/captured history hold pair; at most one exists at a time.
#[derive(Debug, Default)]
pub struct HistoryHold {
    stage: Mutex<HoldStage>,
}

impl HistoryHold {
    /// v2 `holdNextHistoryResponse`: arm the hold; the id is what release and
    /// drop name.
    pub(super) fn arm(&self, session_id: &str) -> Result<String, String> {
        let mut stage = lock(&self.stage);
        if !matches!(*stage, HoldStage::Idle) {
            return Err("terminal peer history hold already owns a response".to_owned());
        }
        let hold_id = mint_uuid().map_err(|error| error.to_string())?;
        *stage = HoldStage::Pending {
            session_id: session_id.to_owned(),
            hold_id: hold_id.clone(),
            deliver: None,
        };
        tracing::info!(session_id, %hold_id, "a direct history response hold was armed");
        Ok(hold_id)
    }

    /// v2 `holdPeerHistoryResponse`: whether this session's read may be
    /// delivered. A read of another session, or with nothing armed, passes.
    pub async fn delivers(&self, session_id: &str) -> bool {
        let decided = {
            let mut stage = lock(&self.stage);
            match &*stage {
                HoldStage::Pending {
                    session_id: armed, ..
                } if armed == session_id => {}
                _ => return true,
            }
            let HoldStage::Pending {
                hold_id, deliver, ..
            } = std::mem::take(&mut *stage)
            else {
                return true;
            };
            if let Some(deliver) = deliver {
                return deliver;
            }
            let (decide, decided) = oneshot::channel();
            *stage = HoldStage::Captured { hold_id, decide };
            decided
        };
        tracing::info!(session_id, "a direct history response is held");
        // A hold torn down without a decision delivers nothing across its
        // test boundary.
        decided.await.unwrap_or(false)
    }

    /// v2 `releaseHistoryResponse`: decide the hold `hold_id` names. An id
    /// that is not the current hold is ignored.
    pub(super) fn decide(&self, hold_id: &str, deliver: bool) {
        let mut stage = lock(&self.stage);
        match &mut *stage {
            HoldStage::Pending {
                hold_id: armed,
                deliver: decision,
                ..
            } if armed == hold_id => *decision = Some(deliver),
            HoldStage::Captured { hold_id: armed, .. } if armed == hold_id => {
                if let HoldStage::Captured { decide, .. } = std::mem::take(&mut *stage) {
                    // The read may have gone away with its port; nothing waits.
                    let _ = decide.send(deliver);
                }
            }
            _ => return,
        }
        tracing::info!(
            hold_id,
            deliver,
            "a direct history response hold was decided"
        );
    }

    /// v2 `dispose`: forget an armed hold and drop a captured one.
    pub(super) fn clear(&self) {
        if let HoldStage::Captured { decide, .. } = std::mem::take(&mut *lock(&self.stage)) {
            let _ = decide.send(false);
        }
    }
}
