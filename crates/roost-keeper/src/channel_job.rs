//! A channel's process tree on Windows: a kill-on-close Job Object holding the
//! PTY's child, so ending the channel ends every process the shell started,
//! the role a process group plays on Unix. Owned by `pty_channel::PtyChannel`.
//! portable-pty does not create the child suspended, so a grandchild spawned in
//! the microseconds before assignment can escape the job; that is accepted.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

/// The job a channel's child runs in. Dropping it closes the last handle, and
/// `KILL_ON_JOB_CLOSE` then ends every process still inside.
#[derive(Debug)]
pub(crate) struct ChannelJob {
    job: OwnedHandle,
}

impl ChannelJob {
    /// A kill-on-close job holding `child`.
    pub(crate) fn holding(child: RawHandle) -> std::io::Result<Self> {
        // SAFETY: both pointer arguments may be null (default security, no
        // name); the call has no other preconditions.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` was just returned by CreateJobObjectW and is owned by
        // nothing else, so the OwnedHandle is its single closer.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let info_len = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| std::io::Error::other("job limit structure exceeds u32"))?;
        // SAFETY: `job` is a live job handle; `info` is an initialised
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION borrowed for the call, and the
        // length passed is exactly its size.
        let limited = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                info_len,
            )
        };
        if limited == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `job` is a live job handle, and `child` is the process handle
        // portable-pty holds open for the child's lifetime, which outlasts this
        // call because the caller owns that child.
        let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child) };
        if assigned == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { job })
    }

    /// End every process in the job now.
    pub(crate) fn terminate(&self) {
        // SAFETY: `self.job` is a live job handle owned by this value.
        let ended = unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        if ended == 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "keeper: the channel's job could not be terminated"
            );
        }
    }
}
