//! Runs one shell command to exit, cancellation, or timeout and captures its output.
//! A foreground `bash` call and a background job both drive this process; the live
//! sink forwards chunks to a caller only while that caller is still attached.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;

use crate::{bash_capture::BashCapture, outcome::ToolOutcome, search_path::extended_path_value};

const STREAM_CHUNK: usize = 8 * 1024;

/// Forwards live output to the call that started the process. `detach` cuts the
/// stream, so a call that has returned or backgrounded receives no more frames.
#[derive(Clone)]
pub struct LiveSink(Arc<Mutex<Option<mpsc::Sender<String>>>>);

impl LiveSink {
    pub fn attached(out: mpsc::Sender<String>) -> Self {
        Self(Arc::new(Mutex::new(Some(out))))
    }

    pub fn detached() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    pub async fn detach(&self) {
        self.0.lock().await.take();
    }

    async fn send(&self, chunk: &[u8]) {
        let sender = self.0.lock().await.as_ref().cloned();
        if let Some(sender) = sender {
            let _ = sender
                .send(String::from_utf8_lossy(chunk).into_owned())
                .await;
        }
    }
}

pub struct BashProcess {
    pub data_dir: PathBuf,
    pub cwd: PathBuf,
    pub script: String,
    pub timeout: Duration,
    pub capture: Arc<Mutex<BashCapture>>,
    pub live: LiveSink,
    pub cancel: CancellationToken,
}

/// After the shell exits, output still in the pipes is read until it goes
/// quiet for this long. A backgrounded grandchild that inherited the pipes
/// holds them open indefinitely, so the settlement must not wait for EOF.
const POST_EXIT_IDLE: Duration = Duration::from_millis(500);
/// The longest the post-exit read may take, so a grandchild that keeps
/// writing cannot hold the result back either.
const POST_EXIT_CAP: Duration = Duration::from_secs(5);

pub async fn run_bash_process(process: BashProcess) -> ToolOutcome {
    let started = Instant::now();
    let mut command = shell_command(&process.script, &process.cwd, &process.data_dir);
    command
        .current_dir(&process.cwd)
        .env("PATH", extended_path_value(&process.cwd, &process.data_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return ToolOutcome::failure(format!("failed to start shell: {error}")),
    };
    let Some(stdout) = child.stdout.take() else {
        return ToolOutcome::failure("failed to capture shell output");
    };
    let Some(stderr) = child.stderr.take() else {
        return ToolOutcome::failure("failed to capture shell output");
    };
    let (chunks_tx, mut chunks_rx) = mpsc::channel::<Vec<u8>>(32);
    tokio::spawn(pump(stdout, chunks_tx.clone()));
    tokio::spawn(pump(stderr, chunks_tx));

    let deadline = tokio::time::sleep(process.timeout);
    tokio::pin!(deadline);
    let mut timed_out = false;
    let mut cancelled = false;
    let mut chunks_open = true;
    let status = loop {
        tokio::select! {
            _ = process.cancel.cancelled() => {
                cancelled = true;
                terminate_child(&mut child).await;
                break None;
            }
            _ = &mut deadline => {
                timed_out = true;
                terminate_child(&mut child).await;
                break None;
            }
            chunk = chunks_rx.recv(), if chunks_open => match chunk {
                Some(chunk) => {
                    if let Err(error) = record_chunk(&process, &chunk).await {
                        terminate_child(&mut child).await;
                        return ToolOutcome::failure(error);
                    }
                }
                None => chunks_open = false,
            },
            result = child.wait() => { break result.ok(); }
        }
    };
    let drained = tokio::time::timeout(POST_EXIT_CAP, async {
        while let Ok(Some(chunk)) = tokio::time::timeout(POST_EXIT_IDLE, chunks_rx.recv()).await {
            record_chunk(&process, &chunk).await?;
        }
        Ok::<(), String>(())
    })
    .await;
    if let Ok(Err(error)) = drained {
        return ToolOutcome::failure(error);
    }
    let (content, artifact_path) = {
        let capture = process.capture.lock().await;
        (capture.content(), capture.artifact_path())
    };
    let timeout = process.timeout;
    let details = serde_json::json!({
        "exit_code": status.as_ref().and_then(|status| status.code()),
        "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "timed_out": timed_out,
        "cancelled": cancelled,
        "truncated": artifact_path.is_some(),
        "artifact_path": artifact_path,
        "artifact_complete": false,
    });
    if timed_out {
        return ToolOutcome::failure(format!(
            "command timed out after {} ms\n{content}",
            timeout.as_millis()
        ))
        .with_details(details);
    }
    if cancelled {
        return ToolOutcome::failure(format!("command cancelled\n{content}")).with_details(details);
    }
    match status {
        Some(status) => {
            if status.success() {
                ToolOutcome::success(content).with_details(details)
            } else {
                ToolOutcome::failure(format!(
                    "exit {}\n{content}",
                    status
                        .code()
                        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                ))
                .with_details(details)
            }
        }
        None => ToolOutcome::failure(format!("command failed\n{content}")).with_details(details),
    }
}

async fn record_chunk(process: &BashProcess, chunk: &[u8]) -> Result<(), String> {
    process.capture.lock().await.push(chunk).await?;
    process.live.send(chunk).await;
    Ok(())
}

fn shell_command(script: &str, _cwd: &Path, _data_dir: &Path) -> Command {
    #[cfg(unix)]
    {
        let mut command = Command::new("bash");
        command.args(["-lc", script]);
        command
    }
    #[cfg(windows)]
    {
        if let Some(binary) = crate::search_path::resolve_binary("bash", _cwd, _data_dir) {
            let mut command = Command::new(binary);
            command.args(["-lc", script]);
            command
        } else {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-Command", script]);
            command
        }
    }
}

async fn pump<R>(mut reader: R, sender: mpsc::Sender<Vec<u8>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = vec![0; STREAM_CHUNK];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) if sender.send(buffer[..count].to_vec()).await.is_err() => break,
            Ok(_) => {}
        }
    }
}

async fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status()
            .await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}
