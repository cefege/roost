//! Retains or removes source checkout changes without mutating the source index or files.
//! The worktree owner uses this for keep-changes fallback and clean clone reconciliation.
//! Git lists paths; filesystem operations preserve symlinks and refuse special nodes.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    Error,
    git::{copy_index, git_bytes},
};

pub(super) fn remove_source_untracked(root: &Path, target: &Path) -> Result<(), Error> {
    let paths = git_bytes(
        root,
        [
            OsStr::new("ls-files"),
            OsStr::new("--others"),
            OsStr::new("--exclude-standard"),
            OsStr::new("-z"),
        ],
    )?;
    for raw in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        remove_tree_entry(&target.join(path_from_git_bytes(raw)?))?;
    }
    Ok(())
}

pub(super) fn copy_live_state(root: &Path, target: &Path) -> Result<(), Error> {
    let tracked = git_bytes(
        root,
        [
            OsStr::new("diff"),
            OsStr::new("--name-only"),
            OsStr::new("-z"),
            OsStr::new("HEAD"),
        ],
    )?;
    let untracked = git_bytes(
        root,
        [
            OsStr::new("ls-files"),
            OsStr::new("--others"),
            OsStr::new("--exclude-standard"),
            OsStr::new("-z"),
        ],
    )?;
    for raw in tracked
        .split(|byte| *byte == 0)
        .chain(untracked.split(|byte| *byte == 0))
        .filter(|path| !path.is_empty())
    {
        let relative = path_from_git_bytes(raw)?;
        let source = root.join(&relative);
        let destination = target.join(&relative);
        match fs::symlink_metadata(&source) {
            Ok(_) => copy_plain_one(&source, &destination)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                remove_tree_entry(&destination)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    copy_index(root, target)?;
    Ok(())
}

fn copy_plain_one(source: &Path, target: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_tree_entry(target)?;
    if metadata.file_type().is_symlink() {
        copy_symlink(source, target)?;
    } else if metadata.is_file() {
        fs::copy(source, target)?;
        fs::set_permissions(target, metadata.permissions())?;
    } else if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_plain_one(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::set_permissions(target, metadata.permissions())?;
    } else {
        return Err(Error::Git(format!(
            "cannot copy special file {}",
            source.display()
        )));
    }
    Ok(())
}

fn remove_tree_entry(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(Error::Io)
        }
        Ok(_) => fs::remove_file(path).map_err(Error::Io),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, Error> {
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, Error> {
    String::from_utf8(bytes.to_vec())
        .map(PathBuf::from)
        .map_err(|error| Error::Git(error.to_string()))
}

#[cfg(unix)]
pub(super) fn copy_symlink(source: &Path, target: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(fs::read_link(source)?, target).map_err(Error::Io)
}

#[cfg(windows)]
pub(super) fn copy_symlink(source: &Path, target: &Path) -> Result<(), Error> {
    let link = fs::read_link(source)?;
    let metadata = fs::metadata(source)?;
    if metadata.is_dir() {
        std::os::windows::fs::symlink_dir(link, target)?;
    } else {
        std::os::windows::fs::symlink_file(link, target)?;
    }
    Ok(())
}
