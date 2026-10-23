//! Copy-on-write staging and directory metadata preservation for linked checkouts.
//! The checkout owner installs the private staging tree only after Git registers its destination.
//! Special nodes are rejected before opening; symlinks are copied without following them.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::{Error, state::copy_symlink};

pub(super) fn clone_source_tree(root: &Path) -> Result<tempfile::TempDir, Error> {
    let parent = root
        .parent()
        .ok_or_else(|| Error::Git("source repository has no parent".into()))?;
    let staging = tempfile::Builder::new()
        .prefix(".roost-worktree-clone-")
        .tempdir_in(parent)?;
    clone_tree_contents(root, staging.path(), true)?;
    Ok(staging)
}

pub(super) fn install_staging(staging: &Path, target: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        fs::rename(entry.path(), target.join(entry.file_name()))?;
    }
    Ok(())
}

fn clone_tree_contents(source: &Path, target: &Path, exclude_git: bool) -> Result<(), Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if exclude_git && entry.file_name() == OsStr::new(".git") {
            continue;
        }
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)?;
        let target_path = target.join(entry.file_name());
        if metadata.file_type().is_symlink() {
            copy_symlink(&source_path, &target_path)?;
        } else if metadata.is_dir() {
            fs::create_dir(&target_path)?;
            clone_tree_contents(&source_path, &target_path, false)?;
        } else if metadata.is_file() {
            reflink_copy::reflink(&source_path, &target_path)?;
            fs::set_permissions(&target_path, metadata.permissions())?;
        } else {
            return Err(Error::Git(format!(
                "cannot clone special file {}",
                source_path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn copy_directory_modes(source: &Path, target: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            let target_path = target.join(entry.file_name());
            if fs::symlink_metadata(&target_path).is_ok_and(|target_metadata| {
                target_metadata.is_dir() && !target_metadata.file_type().is_symlink()
            }) {
                copy_directory_modes(&source_path, &target_path)?;
                fs::set_permissions(target_path, metadata.permissions())?;
            }
        }
    }
    Ok(())
}
