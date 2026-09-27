//! The web bundle an install carries beside its executables, and the one
//! question every install path asks about it before it writes a definition.
//!
//! Called by the first-run, join, deploy and self-update paths. Depends on
//! `roost-host`'s release-directory resolution and on this group's own install
//! seam, and on nothing else. It renders no definition and starts no service:
//! the coordinator and the worker's local door both read the directory this
//! module puts beside `bin/`.
//!
//! **Beside the binaries, not beside the service definition.** A definition
//! lives in the account's unit directory and a release lives in the version
//! tree, and only the version tree is replaced as a unit. A bundle beside the
//! unit therefore outlives the release that shipped it and keeps serving a
//! retired release's page, which is how an install ends up reporting a healthy
//! `spa:` line for a UI it stopped running a fortnight ago. Inside the release
//! directory, retiring the release retires the bundle with it — which is the
//! behaviour `roost status` already separates `web_dist_present` from `serves`
//! for, and why those two fields stay separate after this lands.
//!
//! **`index.html` is the whole test.** A bundle directory without it is a
//! directory of assets no router can answer a deep link from, and a coordinator
//! asked to serve it answers 404 for every URL — which reads as a broken
//! install rather than as the missing file it is.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, ProtocolResult};

use crate::services::install::{InstallError, release_bin_dir};

/// The directory name a release's bundle is installed under, beside `bin/`.
pub const WEB_DIR_NAME: &str = "web";

/// The file every bundle must contain, and the one the router falls back to
/// for a deep link. The status readout's `spa:` line asks about this same file,
/// and imports this name rather than restating it.
pub const WEB_INDEX: &str = "index.html";

/// Where a release's bundle lives, given the `bin` directory that release
/// installed its executables into.
pub fn release_web_dir(bin_dir: &Path) -> PathBuf {
    bin_dir
        .parent()
        .map_or_else(|| bin_dir.join(WEB_DIR_NAME), |root| root.join(WEB_DIR_NAME))
}

/// Where this build's bundle belongs on this machine.
pub fn release_web_dir_for(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    Ok(release_web_dir(&release_bin_dir(env, platform)?))
}

/// What one bundle install copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBundle {
    /// The directory the bundle now lives in.
    pub root: PathBuf,
    /// How many files it holds, which is the number a readout can report.
    pub files: usize,
}

/// Copy a bundle from `source` over `destination`, and report what landed.
///
/// The destination is replaced rather than merged. A merge leaves the previous
/// release's hashed asset files beside the new one's, and an index still
/// referencing a file name the new build does not ship is a page that loads its
/// shell and never its code.
pub fn install_from_dir(source: &Path, destination: &Path) -> Result<InstalledBundle, InstallError> {
    validate(source)?;
    let parent = parent_of(destination)?;
    std::fs::create_dir_all(parent).map_err(|error| InstallError::Io {
        path: parent.to_path_buf(),
        cause: error.to_string(),
    })?;
    let staging = staging_dir(parent);
    let _ = std::fs::remove_dir_all(&staging);
    let files = copy_tree(source, &staging)?;
    replace_into(&staging, destination, files)
}

/// Unpack a release's `roost-web.tar.gz` over `destination`.
///
/// The archive is extracted into a scratch directory and the bundle root is
/// located by its `index.html` rather than by a fixed prefix: the asset's
/// internal layout is the release pipeline's business, and an install that
/// hard-codes one spelling is an install that 404s every URL the day the
/// pipeline wraps the bundle in a directory.
pub fn install_from_tarball(
    archive: &Path,
    destination: &Path,
) -> Result<InstalledBundle, InstallError> {
    let parent = parent_of(destination)?;
    std::fs::create_dir_all(parent).map_err(|error| InstallError::Io {
        path: parent.to_path_buf(),
        cause: error.to_string(),
    })?;
    let extract = parent.join(format!(".{WEB_DIR_NAME}.extracted"));
    let staging = staging_dir(parent);
    let _ = std::fs::remove_dir_all(&extract);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&extract).map_err(|error| InstallError::Io {
        path: extract.clone(),
        cause: error.to_string(),
    })?;
    if let Err(failure) = extract_archive(archive, &extract) {
        let _ = std::fs::remove_dir_all(&extract);
        return Err(failure);
    }
    let root = match locate_bundle_root(&extract, archive) {
        Ok(root) => root,
        Err(failure) => {
            let _ = std::fs::remove_dir_all(&extract);
            return Err(failure);
        }
    };
    std::fs::rename(&root, &staging).map_err(|error| InstallError::Io {
        path: root,
        cause: format!("cannot stage the unpacked bundle: {error}"),
    })?;
    let _ = std::fs::remove_dir_all(&extract);
    let files = count_files(&staging);
    replace_into(&staging, destination, files)
}

/// Refuse a directory that is not a bundle an operator can serve.
///
/// Checked before anything is written, so `--web-dist` naming a `dist` that was
/// never built is a refusal naming the file, rather than an install that
/// reports success and then answers 404 for every URL.
pub fn validate(source: &Path) -> Result<(), InstallError> {
    if !source.is_dir() || !source.join(WEB_INDEX).is_file() {
        return Err(InstallError::NotABundle {
            path: source.to_path_buf(),
            index: WEB_INDEX,
        });
    }
    Ok(())
}

/// Put a fully staged directory in the destination's place.
///
/// The swap is two renames within one directory, so a copy interrupted halfway
/// leaves the previous bundle serving rather than a half-written one that
/// answers 404 for exactly the files it lost.
fn replace_into(
    staging: &Path,
    destination: &Path,
    files: usize,
) -> Result<InstalledBundle, InstallError> {
    let previous = destination
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join(format!(".{WEB_DIR_NAME}.replaced"));
    let _ = std::fs::remove_dir_all(&previous);
    if destination.exists() {
        std::fs::rename(destination, &previous).map_err(|error| InstallError::Io {
            path: destination.to_path_buf(),
            cause: format!("cannot move the previous bundle aside: {error}"),
        })?;
    }
    if let Err(error) = std::fs::rename(staging, destination) {
        // Put the previous bundle back before reporting. A machine whose
        // coordinator answers from a half-copied directory fails every URL at
        // once, and the operator reading the refusal has no way back except a
        // reinstall they now have to know they need.
        let _ = std::fs::rename(&previous, destination);
        return Err(InstallError::Io {
            path: destination.to_path_buf(),
            cause: error.to_string(),
        });
    }
    let _ = std::fs::remove_dir_all(&previous);
    Ok(InstalledBundle {
        root: destination.to_path_buf(),
        files,
    })
}

/// The directory an install stages a bundle in before it takes the
/// destination's place. A sibling, so the final step is a rename within one
/// filesystem and never a cross-device copy of a directory tree.
fn staging_dir(parent: &Path) -> PathBuf {
    parent.join(format!(".{WEB_DIR_NAME}.incoming"))
}

fn parent_of(destination: &Path) -> Result<&Path, InstallError> {
    destination.parent().ok_or_else(|| InstallError::Io {
        path: destination.to_path_buf(),
        cause: "the bundle destination has no parent directory".to_string(),
    })
}

/// Copy every file under `from` into `to`, and count them.
///
/// Symlinks are neither followed nor created. A bundle is a directory of static
/// assets emitted by a bundler, which produces no links, so a link is either
/// an accident or an attempt to place a file outside the destination.
fn copy_tree(from: &Path, to: &Path) -> Result<usize, InstallError> {
    std::fs::create_dir_all(to).map_err(|error| InstallError::Io {
        path: to.to_path_buf(),
        cause: error.to_string(),
    })?;
    let mut copied = 0;
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((source_dir, target_dir)) = pending.pop() {
        let entries = std::fs::read_dir(&source_dir).map_err(|error| InstallError::Io {
            path: source_dir.clone(),
            cause: error.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| InstallError::Io {
                path: source_dir.clone(),
                cause: error.to_string(),
            })?;
            let kind = entry.file_type().map_err(|error| InstallError::Io {
                path: entry.path(),
                cause: error.to_string(),
            })?;
            let target = target_dir.join(entry.file_name());
            if kind.is_dir() {
                // Created here, not when the directory is popped: a bundle's
                // assets live one or two levels down and `std::fs::copy` does
                // not create the parent it is writing into.
                std::fs::create_dir_all(&target).map_err(|error| InstallError::Io {
                    path: target.clone(),
                    cause: error.to_string(),
                })?;
                pending.push((entry.path(), target));
            } else if kind.is_file() {
                std::fs::copy(entry.path(), &target).map_err(|error| InstallError::Io {
                    path: entry.path(),
                    cause: error.to_string(),
                })?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

/// Unpack the archive, refusing a `tar` that could not read it.
///
/// `tar` is run rather than a reader crate because this is the only place the
/// CLI unpacks anything, and the machine that may need it is one with nothing
/// on it but a service manager.
fn extract_archive(archive: &Path, into: &Path) -> Result<(), InstallError> {
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| InstallError::Io {
            path: archive.to_path_buf(),
            cause: format!("cannot run tar to unpack the web bundle: {error}"),
        })?;
    if !output.status.success() {
        return Err(InstallError::Io {
            path: archive.to_path_buf(),
            cause: format!(
                "tar could not unpack the web bundle: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// The directory inside an unpacked archive that is the bundle, or the refusal
/// naming the archive when it is not there.
///
/// Two layouts are accepted and only two: the archive's own root, or a single
/// directory under it. A deeper or ambiguous layout is refused rather than
/// guessed at, because guessing picks one of several candidate roots and the
/// operator's page is whichever one the guess did not pick.
fn locate_bundle_root(extract: &Path, archive: &Path) -> Result<PathBuf, InstallError> {
    if extract.join(WEB_INDEX).is_file() {
        return Ok(extract.to_path_buf());
    }
    let mut roots: Vec<PathBuf> = std::fs::read_dir(extract)
        .map_err(|error| InstallError::Io {
            path: extract.to_path_buf(),
            cause: error.to_string(),
        })?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(WEB_INDEX).is_file())
        .collect();
    if roots.len() == 1 {
        return Ok(roots.remove(0));
    }
    Err(InstallError::NotABundle {
        path: archive.to_path_buf(),
        index: WEB_INDEX,
    })
}

/// How many files a staged directory holds, for the count a readout reports.
fn count_files(root: &Path) -> usize {
    let mut counted = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind) if kind.is_file() => counted += 1,
                _ => {}
            }
        }
    }
    counted
}
