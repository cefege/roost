//! Shell command execution for worker-side tool calls.
//! Called by `ToolHost` with a conversation directory and cancellation token.
//! Uses the shared worker search path and bounds retained output in memory.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use getrandom::fill;
use roost_protocol::wire::agent_chat::BashArgs;
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{outcome::ToolOutcome, search_path::extended_path_value};

const OUTPUT_LIMIT: usize = 50 * 1024;
const STREAM_CHUNK: usize = 8 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT_SECS: u64 = 3600;

pub async fn bash_tool(
    data_dir: &Path,
    cwd: &Path,
    args: BashArgs,
    timeout_ms: u32,
    out: &mpsc::Sender<String>,
    cancel: CancellationToken,
) -> ToolOutcome {
    let timeout = args
        .timeout
        .map(|seconds| Duration::from_secs(seconds.clamp(1, MAX_TIMEOUT_SECS)))
        .unwrap_or_else(|| {
            if timeout_ms == 0 {
                DEFAULT_TIMEOUT
            } else {
                Duration::from_millis(u64::from(timeout_ms))
            }
        });
    let mut command = shell_command(&args.command, cwd, data_dir);
    command
        .current_dir(cwd)
        .env("PATH", extended_path_value(cwd, data_dir))
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

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut retained = Vec::with_capacity(OUTPUT_LIMIT);
    let mut spill: Option<SpillFile> = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        tokio::select! {
            _ = cancel.cancelled() => { cancelled = true; terminate_child(&mut child).await; break None; }
            _ = &mut deadline => { timed_out = true; terminate_child(&mut child).await; break None; }
            chunk = chunks_rx.recv() => {
                if let Some(chunk) = chunk {
                    if let Err(error) = retain_tail(data_dir, &chunk, &mut retained, &mut spill).await {
                        terminate_child(&mut child).await;
                        return ToolOutcome::failure(error);
                    }
                    let _ = out.send(String::from_utf8_lossy(&chunk).into_owned()).await;
                }
                else if let Ok(Some(status)) = child.try_wait() { break Some(status); }
                else { tokio::task::yield_now().await; }
            }
            result = child.wait() => { break result.ok(); }
        }
    };
    while let Some(chunk) = chunks_rx.recv().await {
        if let Err(error) = retain_tail(data_dir, &chunk, &mut retained, &mut spill).await {
            return ToolOutcome::failure(error);
        }
        let _ = out.send(String::from_utf8_lossy(&chunk).into_owned()).await;
    }
    let mut content = String::from_utf8_lossy(&retained).into_owned();
    if let Some(spill_file) = spill {
        content.push_str(&format!(
            "\n[full output saved to {}]",
            spill_file.path.display()
        ));
    }
    if timed_out {
        return ToolOutcome::failure(format!(
            "command timed out after {} ms\n{content}",
            timeout.as_millis()
        ));
    }
    if cancelled {
        return ToolOutcome::failure(format!("command cancelled\n{content}"));
    }
    match status {
        Some(status) if status.success() => ToolOutcome::success(content),
        Some(status) => ToolOutcome::failure(format!(
            "exit {}\n{content}",
            status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
        )),
        None => ToolOutcome::failure(format!("command failed\n{content}")),
    }
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

struct SpillFile {
    path: PathBuf,
    file: tokio::fs::File,
}

async fn retain_tail(
    data_dir: &Path,
    chunk: &[u8],
    retained: &mut Vec<u8>,
    spill: &mut Option<SpillFile>,
) -> Result<(), String> {
    let excess = retained
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(OUTPUT_LIMIT);
    if excess == 0 {
        retained.extend_from_slice(chunk);
        return Ok(());
    }
    let old_excess = excess.min(retained.len());
    append_spill(data_dir, spill, &retained[..old_excess]).await?;
    retained.drain(..old_excess);
    let chunk_excess = excess - old_excess;
    append_spill(data_dir, spill, &chunk[..chunk_excess]).await?;
    retained.extend_from_slice(&chunk[chunk_excess..]);
    Ok(())
}

async fn append_spill(
    data_dir: &Path,
    spill: &mut Option<SpillFile>,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.is_empty() {
        return Ok(());
    }
    if spill.is_none() {
        let directory = data_dir.join("agent-tools").join("spill");
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("failed to create spill directory: {error}"))?;
        let mut random = [0_u8; 16];
        fill(&mut random).map_err(|error| format!("failed to name output spill file: {error}"))?;
        let path = directory.join(format!("{}.log", hex(&random)));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|error| format!("failed to create spill file: {error}"))?;
        *spill = Some(SpillFile { path, file });
    }
    if let Some(spill_file) = spill {
        spill_file
            .file
            .write_all(bytes)
            .await
            .map_err(|error| format!("failed to spill command output: {error}"))?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    value
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn execute(
        script: &str,
        timeout_ms: u32,
        cancel: CancellationToken,
        out: &mpsc::Sender<String>,
    ) -> ToolOutcome {
        let directory = tempdir().expect("tempdir");
        bash_tool(
            directory.path(),
            directory.path(),
            BashArgs {
                command: script.to_owned(),
                timeout: None,
            },
            timeout_ms,
            out,
            cancel,
        )
        .await
    }

    #[tokio::test]
    async fn streams_output_and_prefixes_nonzero_exit() {
        let (out, mut chunks) = mpsc::channel(4);
        let result = execute("printf streamed; exit 7", 0, CancellationToken::new(), &out).await;
        assert!(result.is_error);
        assert!(result.content.starts_with("exit 7\n"));
        assert!(
            chunks
                .recv()
                .await
                .expect("stream chunk")
                .contains("streamed")
        );
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let (out, _) = mpsc::channel(4);
        let result = execute("sleep 5", 50, CancellationToken::new(), &out).await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn cancellation_terminates_command() {
        let (out, _) = mpsc::channel(4);
        let cancellation = CancellationToken::new();
        let child_cancel = cancellation.clone();
        let task = tokio::spawn(async move { execute("sleep 5", 0, child_cancel, &out).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
        let result = task.await.expect("bash task");
        assert!(result.is_error);
        assert!(result.content.contains("cancelled"));
    }
}
