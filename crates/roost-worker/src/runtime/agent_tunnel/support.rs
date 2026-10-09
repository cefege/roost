//! Small pure helpers for tunnel cache names, process pipes, and wire states.
//!
//! The owning module keeps transport state; these helpers only translate the
//! known worker platform and bounded byte-pump operations.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::task::JoinHandle;

use roost_protocol::wire::coord_worker::{
    AgentTunnelOutput, AgentTunnelState, AgentTunnelStateFrame, CoordWorkerUpstream,
};
use sha2::Digest as _;

pub(super) fn write_input(
    mut stdin: ChildStdin,
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    tunnel_id: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(bytes) = receiver.recv().await {
            if let Err(error) = stdin.write_all(&bytes).await {
                tracing::warn!(%tunnel_id, %error, "agent tunnel child stdin closed");
                break;
            }
        }
    })
}

pub(super) fn pump<R: AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    output: tokio::sync::mpsc::Sender<AgentTunnelOutput>,
    tunnel_id: String,
    stderr: bool,
    overflow: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let size = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(size) => size,
            };
            let frame = AgentTunnelOutput {
                tunnel_id: tunnel_id.clone(),
                stderr,
                data: buffer[..size].to_vec(),
            };
            if output.try_send(frame).is_err() {
                overflow.store(true, Ordering::Relaxed);
                break;
            }
        }
    })
}

pub(super) fn state_frame(
    tunnel_id: &str,
    state: AgentTunnelState,
    platform: &str,
    exit_code: i32,
    error: &str,
) -> CoordWorkerUpstream {
    CoordWorkerUpstream::AgentTunnelState(AgentTunnelStateFrame {
        tunnel_id: tunnel_id.to_owned(),
        state,
        platform: platform.to_owned(),
        exit_code,
        error: error.to_owned(),
    })
}

pub(super) fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn home_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

pub(super) fn current_platform() -> Option<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return None;
    };
    Some(format!("{os}-{arch}"))
}
