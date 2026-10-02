//! The direct-input hold: before an authenticated peer's input is admitted,
//! the worker asks the harness's hold socket whether to write it, and the
//! harness may keep the answer back while a test changes the world around
//! the held input. Called by `local_terminal::input`; depends on tokio's Unix
//! socket only. Ports `awaitDirectInputHoldDecision` from
//! `smoke/terminal/stack-direct-input-hold.ts`.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// The most a decision line may carry; more is a broken harness.
const MAX_DECISION_BYTES: usize = 8 * 1024;

/// How long a held input may wait for its decision before it counts as
/// dropped.
const DECISION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize)]
struct HoldRequest<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "sessionId")]
    session_id: &'a str,
    /// Decimal, because the harness reads it as a bigint.
    #[serde(rename = "inputSeq")]
    input_seq: String,
}

#[derive(Deserialize)]
struct HoldDecision {
    action: String,
}

/// The hold socket every peer input is asked about.
#[derive(Debug, Clone)]
pub struct DirectInputHold {
    socket: PathBuf,
}

impl DirectInputHold {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Whether this input may be written. FAILS CLOSED: a timeout, a closed or
    /// failed socket, an oversized or malformed line and an explicit `drop`
    /// all mean the PTY is not written; `pass` and `release` mean it is.
    pub async fn admits(&self, session_id: &str, input_seq: u64) -> bool {
        let decision =
            tokio::time::timeout(DECISION_TIMEOUT, self.ask(session_id, input_seq)).await;
        let admitted = matches!(&decision, Ok(Ok(Some(action))) if action != "drop");
        tracing::debug!(
            session_id,
            input_seq,
            admitted,
            "a held direct input was decided"
        );
        admitted
    }

    /// One connection, one request line, one decision line.
    async fn ask(&self, session_id: &str, input_seq: u64) -> std::io::Result<Option<String>> {
        let mut stream = UnixStream::connect(&self.socket).await?;
        let request = HoldRequest {
            kind: "input",
            session_id,
            input_seq: input_seq.to_string(),
        };
        let mut line = serde_json::to_vec(&request).map_err(std::io::Error::other)?;
        line.push(b'\n');
        stream.write_all(&line).await?;

        let mut buffered = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(None);
            }
            buffered.extend_from_slice(&chunk[..read]);
            if let Some(newline) = buffered.iter().position(|byte| *byte == b'\n') {
                let decision = serde_json::from_slice::<HoldDecision>(&buffered[..newline]).ok();
                return Ok(decision.map(|decision| decision.action));
            }
            if buffered.len() > MAX_DECISION_BYTES {
                return Ok(None);
            }
        }
    }
}
