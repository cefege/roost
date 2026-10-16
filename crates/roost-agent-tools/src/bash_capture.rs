//! Bounded tail of one shell command's output; bytes past the in-memory limit
//! spill to a per-command file. Foreground results and `job output` both read
//! through this type, so a background job reports the same bytes as a call.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use getrandom::fill;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

pub const OUTPUT_LIMIT: usize = 50 * 1024;

#[derive(Debug)]
pub struct BashCapture {
    data_dir: PathBuf,
    retained: Vec<u8>,
    spill: Option<SpillFile>,
}

#[derive(Debug)]
struct SpillFile {
    path: PathBuf,
    file: tokio::fs::File,
}

impl BashCapture {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            retained: Vec::with_capacity(OUTPUT_LIMIT),
            spill: None,
        }
    }

    pub async fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        retain_tail(&self.data_dir, chunk, &mut self.retained, &mut self.spill).await
    }

    /// The retained tail, followed by a pointer to the spill file when earlier output moved there.
    pub fn content(&self) -> String {
        let mut content = String::from_utf8_lossy(&self.retained).into_owned();
        if let Some(path) = self.artifact_path() {
            let _ = write!(content, "\n[earlier output saved to {path}]");
        }
        content
    }

    pub fn artifact_path(&self) -> Option<String> {
        self.spill
            .as_ref()
            .map(|file| file.path.display().to_string())
    }
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
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    value
}
