//! Step 4 of the worker boot: opening the durable outbox and saying what it
//! holds. `runtime::boot_sequence::run` is the only caller, and it attaches the
//! returned journal to the link so the barrier resumes at its high water mark.
//! Depends on `runtime::Journal` and nothing that depends on it back.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;

use super::{DATABASE_FILE_NAME, Journal};

/// Open the outbox under `data_dir`, returning it with the path it was opened
/// at so a later refusal can name the file.
///
/// A store that cannot be opened is a boot refusal, not a warning: a worker
/// that accepted sessions it could not record would leave the coordinator
/// believing a dead session is alive.
pub(super) async fn open_outbox(data_dir: &Path) -> anyhow::Result<(Arc<Journal>, PathBuf)> {
    let outbox_path = data_dir.join(DATABASE_FILE_NAME);
    let outbox = Arc::new(Journal::open(&outbox_path).await.with_context(|| {
        format!(
            "the durable outbox at {} could not be opened",
            outbox_path.display()
        )
    })?);
    // Logged, not propagated: a stats read that fails says the store is
    // answering, which is the only thing the line is for. The open above
    // already refused anything that is not.
    match outbox.stats().await {
        Ok(stats) => tracing::info!(
            path = %outbox_path.display(),
            rows = stats.rows,
            resumed_at = outbox.handed_over_at(),
            "the durable outbox is open and the barrier resumes at its high water mark"
        ),
        Err(error) => tracing::warn!(
            path = %outbox_path.display(),
            %error,
            "the durable outbox is open but its row count could not be read"
        ),
    }
    Ok((outbox, outbox_path))
}
