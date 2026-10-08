//! The single home of Win32 FFI that crates other than the keeper need: the
//! peer process of a named pipe, the volume/file-index identity of a file, and
//! the inherit flag on this process's standard handles. Every other crate
//! forbids `unsafe`, so they call these safe wrappers. Used by roost-worker's
//! boot, agent-report server and agent-install proofs; depends on `windows-sys`.

use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    GetFileInformationByHandle,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;

/// Clear the inherit flag on this process's standard handles.
///
/// `CreateProcess` with inheritance on hands a child every inheritable handle,
/// not just the stdio it was given, so a keeper started by a worker whose
/// stdout is a launcher's `>>` log file holds that file open for its whole
/// life, and the next launcher's redirect then fails with a sharing violation.
/// Children that ask for inherited stdio still get it: std duplicates the
/// standard handle as inheritable for each such spawn.
pub fn disinherit_standard_handles() -> std::io::Result<()> {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle takes a constant selector and only reads the
        // process parameter block.
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is this process's live standard handle, and only
        // its inherit bit is changed.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Process id of the client connected to a named-pipe server handle.
pub fn named_pipe_client_process_id(pipe: &impl AsRawHandle) -> std::io::Result<u32> {
    let mut pid = 0_u32;
    // SAFETY: the handle is borrowed from a live pipe for the call's duration,
    // and `pid` is a valid, exclusively borrowed u32 the call writes once.
    let answered = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) };
    if answered == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(pid)
}

/// (volume serial, file index) of an open file: the Windows analogue of
/// (st_dev, st_ino).
pub fn open_file_identity(file: &std::fs::File) -> std::io::Result<(u64, u64)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` for the call, and
    // `info` is an initialised structure the call fills.
    let answered = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if answered == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

/// (volume serial, file index) of `path`; `follow_links = false` identifies a
/// link itself.
pub fn path_identity(path: &std::path::Path, follow_links: bool) -> std::io::Result<(u64, u64)> {
    let link_flag = if follow_links {
        0
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    };
    // Access mode 0 asks for metadata only, and BACKUP_SEMANTICS is what lets
    // CreateFile open a directory.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | link_flag)
        .open(path)?;
    open_file_identity(&file)
}
