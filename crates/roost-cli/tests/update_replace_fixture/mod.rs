//! The throwaway install directory and the helpers the self-replace tests
//! read it with, shared by the two files that assert against it from opposite
//! sides: `update_replace_bytes.rs` asks what BYTES the swap installs, and
//! `update_self_replace.rs` asks what else it leaves on disk and what it
//! refuses.
//!
//! The property underneath both is the one a reader cannot get from the code: a
//! self-replace that fails leaves the INSTALLED BINARY byte-identical and a
//! journal naming where the previous copy is, and a self-replace that succeeds
//! leaves nothing beside the target but the new binary. Both are claims about
//! bytes and inodes on a filesystem, so every case drives the real
//! `replace_executable` against a real directory and then reads the directory
//! back.
//!
//! The negative cases matter more than the positive one. A function that
//! returned `Ok` and had written the target with `fs::write` passes a test that
//! only checks the happy path, and the machine that finds out is one whose
//! `roost` was truncated by a power cut.

#![allow(clippy::unwrap_used, clippy::expect_used)]

// The fixture's own needs, imported privately. The types each TEST file needs
// are imported there instead: a re-export used by one file and not the other
// is an unused import in the file that does not, and two binaries of the same
// fixture wanting different halves of it is the point of splitting them.
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub use roost_cli::services::deploy_journal::sha256_hex;
use roost_cli::update::candidate::VerifiedCandidate;
use roost_cli::update::journal::KeeperRecord;
use roost_cli::update::rollout::InstalledBinary;

/// A throwaway install directory, removed when the test ends.
pub struct Install {
    /// The tree, kept so `Drop` can remove it.
    pub root: PathBuf,
}

impl Install {
    pub fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-update-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("service")).expect("the throwaway tree is created");
        Self { root }
    }

    /// The install's `roost`, with the bytes and mode an installer would leave.
    pub fn install_binary(&self, body: &[u8], mode: u32) -> PathBuf {
        let path = self.root.join("roost");
        std::fs::write(&path, body).expect("the fake binary is written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .expect("the fake binary is made executable");
        path
    }

    /// A verified candidate beside the installed binary, as the downloader
    /// leaves it after the digest matched the release's sidecar.
    pub fn candidate(&self, body: &[u8]) -> VerifiedCandidate {
        let path = self.root.join("roost.candidate");
        std::fs::write(&path, body).expect("the candidate is written");
        VerifiedCandidate {
            path,
            sha256: sha256_hex(body),
            url: "https://example.invalid/roost-linux-x64".to_string(),
        }
    }

    pub fn service_dir(&self) -> PathBuf {
        self.root.join("service")
    }

    /// Every name in the install directory, so a test asserts what was left
    /// behind as well as what the target now holds.
    pub fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .expect("the install directory is readable")
            .map(|entry| {
                entry
                    .expect("a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()))
}

pub fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .permissions()
        .mode()
        & 0o7777
}

pub fn inode_of(path: &Path) -> u64 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .ino()
}

pub fn no_keeper() -> Option<KeeperRecord> {
    Some(KeeperRecord::NoRunningKeeper {
        reason: "no coordinator roster on this machine records a keeper here".to_string(),
    })
}

pub fn installed(body: &[u8], mode: u32, keeper: Option<KeeperRecord>) -> InstalledBinary {
    InstalledBinary {
        sha256: sha256_hex(body),
        mode,
        keeper,
    }
}

pub const OLD: &[u8] = b"#!/bin/sh\n# the installed roost\necho old\n";
pub const NEW: &[u8] = b"#!/bin/sh\n# the published roost\necho new\nand more of it\n";
pub const NOW: i64 = 1_781_900_000_000;
pub const VERSION: &str = "v9.9.9";
