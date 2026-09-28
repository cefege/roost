//! A worker's `update-progress` frame: validated as the link's dispatcher
//! validates it, then offered to the deploy job it names. Called by
//! `worker_link::live_frames`; depends on `deploy::jobs`. Ports the
//! `updateProgress` arm of apps/coord/src/workers/worker-frame-dispatch.ts and
//! the admission of its only consumer, `handleWorkerUpdateProgress` in
//! apps/coord/src/deploy/windows-update-deploy-jobs.ts.
//!
//! ONLY A SIGNED WINDOWS UPDATE CONSUMES PROGRESS. A POSIX job's output is its
//! subprocess, so progress naming one is ignored, exactly as v2 ignores it; the
//! signed Windows updater is not carried by this coordinator, so progress for
//! one is refused aloud rather than dropped as if nothing asked for it.

use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::UpdateProgress;

use crate::deploy::DeployRuntime;
use crate::deploy::jobs::is_deploy_job_id;

/// The largest sequence the original's safe-integer check admits.
const MAX_SAFE_SEQUENCE: u64 = (1 << 53) - 1;

impl DeployRuntime {
    /// Accept one progress frame, or the reason the dispatcher refuses it.
    pub fn accept_update_progress(
        &self,
        worker_fp: &WorkerFp,
        progress: &UpdateProgress,
    ) -> Result<(), &'static str> {
        if progress.job_id.is_empty() || progress.sequence > MAX_SAFE_SEQUENCE {
            return Err("invalid_update_progress");
        }
        if !is_deploy_job_id(&progress.job_id) {
            tracing::debug!(%worker_fp, job_id = %progress.job_id,
                "update progress: not a deploy job id; ignored");
            return Ok(());
        }
        if self.journal().job(&progress.job_id).is_some() {
            tracing::debug!(%worker_fp, job_id = %progress.job_id,
                "update progress: names a POSIX deploy job, which reads its subprocess; ignored");
            return Ok(());
        }
        tracing::warn!(%worker_fp, job_id = %progress.job_id, sequence = progress.sequence,
            phase = %progress.phase, terminal = progress.terminal,
            "update progress refused: signed Windows update deploys are not run by this coordinator");
        Ok(())
    }
}
