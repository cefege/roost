//! The Win32 FFI that crates other than the keeper need: clearing the inherit
//! flag on this process's standard handles. Every other crate forbids
//! `unsafe`, so they call this safe wrapper. Used by roost-worker's runtime
//! start; depends on `windows-sys`.

use windows_sys::Win32::Foundation::{
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

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
