//! Removes the source checkout's untracked files from a reflink clone so it matches
//! the target commit; also the filesystem helpers the clone path shares.
//! Git lists paths; filesystem operations preserve symlinks.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Error, git::git_bytes};

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
