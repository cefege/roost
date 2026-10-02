//! The worker's end of the harness's fault command socket: connect once, say
//! hello with this worker's label, then answer newline-delimited JSON commands
//! until the harness goes away. Spawned by `FaultControls::serve_commands`;
//! depends on `commands` for the vocabulary. Ports
//! `startWorkerPeerFaultClient` from
//! `smoke/terminal/stack-peer-fault-worker-client.ts`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use super::PeerFaultState;
use super::commands::{apply_command, parse_command, result_line};

/// The most a single unanswered line may hold; a harness that sends more is
/// broken, and the connection ends rather than buffering it.
const MAX_CONTROL_BYTES: usize = 16 * 1024;

/// Serve the command socket to completion, then clear every fault it armed.
pub(super) async fn serve_fault_commands(
    socket: PathBuf,
    worker_label: String,
    state: Arc<PeerFaultState>,
) {
    let stream = match UnixStream::connect(&socket).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(socket = %socket.display(), %error, "the terminal peer fault socket could not be reached");
            return;
        }
    };
    tracing::info!(socket = %socket.display(), %worker_label, "the terminal peer fault socket is connected");
    let (reader, writer) = stream.into_split();
    // Every line goes through one writer task, so concurrently finishing
    // commands never interleave their bytes.
    let (lines, outbox) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(write_lines(writer, outbox));
    let mut hello = serde_json::json!({ "type": "hello", "workerLabel": worker_label })
        .to_string()
        .into_bytes();
    hello.push(b'\n');
    if lines.send(hello).is_ok() {
        read_commands(reader, &lines, &state).await;
    }
    drop(lines);
    writer_task.abort();
    state.dispose();
    tracing::info!("the terminal peer fault socket closed");
}

/// Read commands until the harness closes, a line is not a command, or the
/// buffer overflows. Each command runs on its own task, as v2 dispatches
/// them, so a long hold never blocks the commands behind it.
async fn read_commands(
    mut reader: tokio::net::unix::OwnedReadHalf,
    lines: &mpsc::UnboundedSender<Vec<u8>>,
    state: &Arc<PeerFaultState>,
) {
    let mut buffered: Vec<u8> = Vec::new();
    let mut chunk = vec![0_u8; 4096];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        if buffered.len() + read > MAX_CONTROL_BYTES {
            tracing::warn!("a terminal peer fault line exceeded its bound; the socket is closed");
            return;
        }
        buffered.extend_from_slice(&chunk[..read]);
        while let Some(newline) = buffered.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffered.drain(..=newline).collect();
            let Some(command) = parse_command(&line[..newline]) else {
                tracing::warn!(
                    "a terminal peer fault line was not a command; the socket is closed"
                );
                return;
            };
            let state = Arc::clone(state);
            let lines = lines.clone();
            tokio::spawn(async move {
                let outcome = apply_command(&state, &command).await;
                if let Err(error) = &outcome {
                    tracing::warn!(%error, "a terminal peer fault command failed");
                }
                let _ = lines.send(result_line(&command.request_id, outcome));
            });
        }
    }
}

async fn write_lines(mut writer: OwnedWriteHalf, mut outbox: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(line) = outbox.recv().await {
        if writer.write_all(&line).await.is_err() {
            return;
        }
    }
}
