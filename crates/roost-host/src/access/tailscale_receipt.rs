//! Durable ownership and interrupted-operation records for Tailscale Serve.
//!
//! The caller supplies a private, existing state directory. The OS lock
//! serializes Roost controllers and is automatically released on process exit.

use super::TailscaleError;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub hostname: String,
    pub port: u16,
    pub proxy: String,
}

pub(in crate::access) struct Store {
    directory: PathBuf,
    _lock: Option<File>,
}

impl Store {
    pub fn snapshot(directory: &Path) -> Result<Self, TailscaleError> {
        validate_parent(directory)?;
        match fs::symlink_metadata(directory) {
            Ok(_) => validate_directory(directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            _lock: None,
        })
    }

    pub fn lock(directory: &Path) -> Result<Self, TailscaleError> {
        validate_parent(directory)?;
        let parent = directory
            .parent()
            .ok_or(TailscaleError::InsecureStateDirectory)?;
        if directory.file_name().and_then(|name| name.to_str()) == Some("tailscale-access") {
            verify_directory_owner(parent)?;
        }
        create_state_directory(directory, parent)?;
        validate_directory(directory)?;
        verify_directory_owner(directory)?;
        let lock_path = directory.join("tailscale-serve.lock");
        validate_file(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|_| TailscaleError::ReceiptIo)?;
        lock.try_lock().map_err(|_| TailscaleError::Busy)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            _lock: Some(lock),
        })
    }

    fn path(&self, pending: bool) -> PathBuf {
        self.directory.join(if pending {
            "tailscale-serve.pending.json"
        } else {
            "tailscale-serve.owned.json"
        })
    }

    pub(super) fn read(&self, pending: bool) -> Result<Option<Receipt>, TailscaleError> {
        validate_file(&self.path(pending))?;
        let bytes = match fs::read(self.path(pending)) {
            Ok(bytes) if bytes.len() <= 4096 => bytes,
            Ok(_) => return Err(TailscaleError::InvalidReceipt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        };
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| TailscaleError::InvalidReceipt)?;
        if receipt.port == 0
            || receipt.hostname.is_empty()
            || receipt.proxy != format!("http://127.0.0.1:{}", receipt.port)
        {
            return Err(TailscaleError::InvalidReceipt);
        }
        Ok(Some(receipt))
    }

    pub(super) fn create(&self, pending: bool, receipt: &Receipt) -> Result<(), TailscaleError> {
        let bytes = serde_json::to_vec(receipt).map_err(|_| TailscaleError::InvalidReceipt)?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| TailscaleError::ReceiptIo)?;
        let suffix = hex::encode(nonce);
        let temporary = self
            .directory
            .join(format!(".tailscale-serve-{suffix}.pending"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| TailscaleError::ReceiptIo)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| TailscaleError::ReceiptIo)?;
        fs::hard_link(&temporary, self.path(pending)).map_err(|_| TailscaleError::ReceiptIo)?;
        fs::remove_file(temporary).map_err(|_| TailscaleError::ReceiptIo)?;
        self.sync_directory()
    }

    pub fn remove(&self, pending: bool) -> Result<(), TailscaleError> {
        validate_file(&self.path(pending))?;
        match fs::remove_file(self.path(pending)) {
            Ok(()) => self.sync_directory(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(TailscaleError::ReceiptIo),
        }
    }

    fn sync_directory(&self) -> Result<(), TailscaleError> {
        #[cfg(unix)]
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| TailscaleError::ReceiptIo)?;
        Ok(())
    }
}

#[cfg(unix)]
fn create_state_directory(directory: &Path, parent: &Path) -> Result<(), TailscaleError> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(directory) {
        Ok(()) => fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| TailscaleError::ReceiptIo),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err(TailscaleError::ReceiptIo),
    }
}

#[cfg(not(unix))]
fn create_state_directory(_directory: &Path, _parent: &Path) -> Result<(), TailscaleError> {
    Err(TailscaleError::InsecureStateDirectory)
}

fn validate_parent(directory: &Path) -> Result<(), TailscaleError> {
    if directory.file_name().and_then(|name| name.to_str()) != Some("tailscale-access") {
        return Ok(());
    }
    let parent = directory
        .parent()
        .ok_or(TailscaleError::InsecureStateDirectory)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| TailscaleError::ReceiptIo)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(TailscaleError::InsecureStateDirectory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(TailscaleError::InsecureStateDirectory);
        }
    }
    for name in [
        "tailscale-serve.pending.json",
        "tailscale-serve.owned.json",
        "tailscale-proxy.receipt",
        "tailscale-proxy.caddyfile",
    ] {
        match fs::symlink_metadata(parent.join(name)) {
            Ok(_) => return Err(TailscaleError::RecoveryRequired),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        }
    }
    Ok(())
}

fn validate_directory(directory: &Path) -> Result<(), TailscaleError> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| TailscaleError::ReceiptIo)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(TailscaleError::InsecureStateDirectory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(TailscaleError::InsecureStateDirectory);
        }
    }
    Ok(())
}

fn validate_file(path: &Path) -> Result<(), TailscaleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(TailscaleError::InvalidReceipt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(TailscaleError::ReceiptIo),
    }
}

#[cfg(unix)]
fn verify_directory_owner(directory: &Path) -> Result<(), TailscaleError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::time::{SystemTime, UNIX_EPOCH};
    let owner = fs::symlink_metadata(directory)
        .map_err(|_| TailscaleError::ReceiptIo)?
        .uid();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TailscaleError::ReceiptIo)?
        .as_nanos();
    let probe = directory.join(format!(".tailscale-owner-{}-{nonce}", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)
        .map_err(|_| TailscaleError::ReceiptIo)?;
    let file_owner = file
        .metadata()
        .map_err(|_| TailscaleError::ReceiptIo)?
        .uid();
    fs::remove_file(probe).map_err(|_| TailscaleError::ReceiptIo)?;
    if owner != file_owner {
        return Err(TailscaleError::InsecureStateDirectory);
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_directory_owner(_directory: &Path) -> Result<(), TailscaleError> {
    // The standard library does not expose Windows ACL ownership validation.
    Err(TailscaleError::InsecureStateDirectory)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn private_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let directory =
            std::env::temp_dir().join(format!("roost-serve-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory).unwrap_or_else(|error| panic!("{error}"));
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("{error}"));
        directory
    }

    #[test]
    fn intent_and_owned_receipts_survive_store_reopen() {
        let directory = private_directory();
        let receipt = Receipt {
            hostname: "node.example.ts.net".into(),
            port: 4113,
            proxy: "http://127.0.0.1:4113".into(),
        };
        {
            let store = Store::lock(&directory).unwrap_or_else(|error| panic!("{error}"));
            store
                .create(true, &receipt)
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(store.read(true).ok().flatten(), Some(receipt.clone()));
        }
        {
            let store = Store::lock(&directory).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(store.read(true).ok().flatten(), Some(receipt.clone()));
            store
                .create(false, &receipt)
                .unwrap_or_else(|error| panic!("{error}"));
            store.remove(true).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(store.read(false).ok().flatten(), Some(receipt));
        }
        fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn snapshot_is_read_only_and_receipt_symlinks_are_rejected() {
        let directory = private_directory();
        let store = Store::snapshot(&directory).unwrap_or_else(|error| panic!("{error}"));
        assert!(store.read(true).ok().flatten().is_none());
        assert!(!directory.join("tailscale-serve.lock").exists());
        symlink(
            "missing-target",
            directory.join("tailscale-serve.pending.json"),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(matches!(
            store.read(true),
            Err(TailscaleError::InvalidReceipt)
        ));
        assert!(matches!(
            store.remove(true),
            Err(TailscaleError::InvalidReceipt)
        ));
        drop(store);
        fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn rejects_nonprivate_directory_and_symlink_lock() {
        let directory = private_directory();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(matches!(
            Store::lock(&directory),
            Err(TailscaleError::InsecureStateDirectory)
        ));
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("{error}"));
        symlink("missing-target", directory.join("tailscale-serve.lock"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(matches!(
            Store::lock(&directory),
            Err(TailscaleError::InvalidReceipt)
        ));
        fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn preview_does_not_create_private_state_under_default_data_directory() {
        let directory = private_directory();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("{error}"));
        let state = directory.join("tailscale-access");
        let snapshot = Store::snapshot(&state).unwrap_or_else(|error| panic!("{error}"));
        assert!(snapshot.read(true).ok().flatten().is_none());
        assert!(!state.exists());
        drop(snapshot);
        let store = Store::lock(&state).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            fs::metadata(&state)
                .unwrap_or_else(|error| panic!("{error}"))
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&directory)
                .unwrap_or_else(|error| panic!("{error}"))
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        drop(store);
        fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("{error}"));
    }
}
