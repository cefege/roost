//! Why a keeper stops without a worker asking: a termination signal, or its
//! socket file disappearing. `Server` polls this from its accept wait and from
//! every connection turn; the daemon installs it. Ports the `SIGTERM` handler
//! and the 30 s socket check of v2 `apps/worker/src/keeper/multiplexed-main.ts`.

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a keeper checks that its socket file still exists, as v2 did.
///
/// No worker can connect to a path that is gone, so a keeper whose socket was
/// deleted holds PTYs nobody can reach, and would otherwise hold them, and
/// every shell in them, until the machine reboots.
pub const SOCKET_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// The longest the accept wait sleeps between looks at the stop flag.
///
/// A signal sent to the process may be handled on any thread that does not
/// block it, and only a handler that ran on the waiting thread interrupts its
/// `poll`. One handled on a PTY reader thread sets the flag without waking the
/// wait, so the wait never sleeps longer than this.
const SIGNAL_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// Why the keeper is stopping on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    /// A termination signal arrived.
    Signalled,
    /// The socket file is gone.
    SocketRemoved,
}

impl ExitCause {
    /// The stop reason the daemon logs.
    pub fn reason(self) -> &'static str {
        match self {
            ExitCause::Signalled => "signalled",
            ExitCause::SocketRemoved => "socket removed",
        }
    }
}

/// The flag a signal handler sets, and the socket path whose disappearance
/// stops the keeper.
#[derive(Debug)]
pub struct ExitWatch {
    signal: &'static AtomicBool,
    socket: PathBuf,
    interval: Duration,
    next_socket_check: Instant,
}

impl ExitWatch {
    /// Stop when `signal` is set, or when `socket` is found missing; the path
    /// is looked at once every `interval`, the flag on every poll.
    pub fn new(signal: &'static AtomicBool, socket: PathBuf, interval: Duration) -> Self {
        Self {
            signal,
            socket,
            interval,
            next_socket_check: Instant::now() + interval,
        }
    }

    /// Whether the keeper should stop now. The path is only touched once its
    /// interval has passed, so this is cheap enough for every connection turn.
    pub fn poll(&mut self) -> Option<ExitCause> {
        if self.signal.load(Ordering::SeqCst) {
            return Some(ExitCause::Signalled);
        }
        let now = Instant::now();
        if now < self.next_socket_check {
            return None;
        }
        self.next_socket_check = now + self.interval;
        (!socket_path_present(&self.socket)).then_some(ExitCause::SocketRemoved)
    }

    /// How long a wait may sleep before this watch needs another look.
    pub(super) fn wait_budget(&self) -> Duration {
        self.next_socket_check
            .saturating_duration_since(Instant::now())
            .min(SIGNAL_CHECK_INTERVAL)
    }
}

/// Whether the socket file is still there. `exists` follows a symlink and reads
/// any error as absence, which is what v2's `fs.existsSync` answered.
#[cfg(unix)]
fn socket_path_present(socket: &std::path::Path) -> bool {
    socket.exists()
}

/// Whether the socket file is still there. A Windows AF_UNIX socket file is a
/// reparse point that cannot be opened through, so following it would read a
/// live socket as gone; the entry itself is looked at instead.
#[cfg(windows)]
fn socket_path_present(socket: &std::path::Path) -> bool {
    std::fs::symlink_metadata(socket).is_ok()
}

/// Wait until `listener` has a connection waiting, or `timeout` passes.
///
/// `false` on a timeout, an interrupting signal, or a failed wait: the caller
/// answers every one of those by polling its watch again.
#[cfg(unix)]
pub(super) fn wait_for_connection(listener: &impl AsRawFd, timeout: Duration) -> bool {
    let mut entry = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `entry` is one initialised `pollfd`, exclusively borrowed for the
    // duration of the call, so the count of 1 matches the memory handed over;
    // its descriptor stays open because the caller holds the listener across
    // the call.
    let ready = unsafe { libc::poll(&mut entry, 1, millis) };
    ready > 0 && entry.revents & libc::POLLIN != 0
}

/// Wait until `listener` has a connection waiting, or `timeout` passes.
///
/// `false` on a timeout or a failed wait: the caller answers both by polling
/// its watch again.
#[cfg(windows)]
pub(super) fn wait_for_connection(
    listener: &impl std::os::windows::io::AsRawSocket,
    timeout: Duration,
) -> bool {
    use windows_sys::Win32::Networking::WinSock::{POLLRDNORM, WSAPOLLFD, WSAPoll};
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let Ok(socket) = usize::try_from(listener.as_raw_socket()) else {
        return false;
    };
    let mut entry = WSAPOLLFD {
        fd: socket,
        events: POLLRDNORM,
        revents: 0,
    };
    // SAFETY: `entry` is one initialised `WSAPOLLFD`, exclusively borrowed for
    // the duration of the call, so the count of 1 matches the memory handed
    // over; its socket stays open because the caller holds the listener across
    // the call.
    let ready = unsafe { WSAPoll(&mut entry, 1, millis) };
    ready > 0 && (entry.revents & POLLRDNORM) != 0
}
