//! Verified daemon cache persistence for worker agent tunnels.
//!
//! The owning module selects a worker data directory and calls these methods
//! only after the uploaded binary's complete digest matches the host manifest.

use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

use super::AgentTunnelOwner;

impl AgentTunnelOwner {
    pub(super) fn cached_daemon_path(&self, platform: &str, sha256: &str) -> PathBuf {
        #[cfg(windows)]
        let filename = format!("pi-env-{}.exe", &sha256[..32]);
        #[cfg(not(windows))]
        let filename = format!("pi-env-{}", &sha256[..32]);
        self.cache.join(platform).join(filename)
    }

    pub(super) async fn prune_daemon_cache(&self, platform: &str, sha256: &str) {
        let keep = self.cached_daemon_path(platform, sha256);
        let Ok(mut entries) = tokio::fs::read_dir(self.cache.join(platform)).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let is_daemon = entry.file_name().to_string_lossy().starts_with("pi-env-");
            if is_daemon
                && path != keep
                && let Err(error) = tokio::fs::remove_file(path).await
            {
                tracing::warn!(platform, %error, "stale agent daemon cache entry could not be removed");
            }
        }
    }

    pub(super) async fn persist_daemon(
        &self,
        platform: &str,
        sha256: &str,
        bytes: &[u8],
    ) -> std::io::Result<PathBuf> {
        let destination = self.cached_daemon_path(platform, sha256);
        tokio::fs::create_dir_all(destination.parent().unwrap_or(&self.cache)).await?;
        let partial = destination.with_extension("part");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&partial)
            .await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = file.metadata().await?.permissions();
            permissions.set_mode(0o700);
            file.set_permissions(permissions).await?;
        }
        drop(file);
        let _ = tokio::fs::remove_file(&destination).await;
        tokio::fs::rename(&partial, &destination).await?;
        Ok(destination)
    }
}
