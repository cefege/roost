//! Delivering one named signal to one pid. Called by `dev/supervisor.rs` when a
//! stack is being stopped, and by nothing else: `status/service_probe.rs` has
//! its own because a probe that timed out wants a different signal.
//!
//! `libc::kill` is an `unsafe` call and this crate forbids `unsafe`, and
//! `std::process::Child::kill` is SIGKILL *and* needs the handle the waiting
//! task owns — so the signal goes out through the `kill` program, the same way
//! the service probe already sends one.

/// What Ctrl-C in a terminal sends.
pub const INTERRUPT: &str = "INT";
/// What a service manager sends, and what `roost coord` and `roost worker`
/// both listen for as well.
pub const TERMINATE: &str = "TERM";
/// The escalation for a child that did not leave after the polite signal. A
/// SIGKILLed coordinator takes its keeper's PTYs with it, so it is the last
/// step of a shutdown and never the first.
pub const KILL: &str = "KILL";

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("could not run the kill program: {0}")]
    NoSignalProgram(String),
    #[error("{signal} to pid {pid} was refused")]
    Refused { pid: u32, signal: &'static str },
    #[error(
        "pid {pid} names no single process: `kill` would read it as a process group or as every process"
    )]
    NotASingleProcess { pid: u32 },
}

/// Send `signal` to `pid`. A pid that has already exited is refused by the
/// kernel, which is the common case in a shutdown and not a failure worth
/// stopping for; the caller decides what a refusal means. A pid `kill` would
/// read as a process group or as every process is refused before anything is
/// sent.
pub fn send(pid: u32, signal: &'static str) -> Result<(), SignalError> {
    // `kill` parses its operand into a signed pid_t: 0 is the caller's own
    // process group, and anything above i32::MAX wraps negative — u32::MAX
    // becomes -1, which is every process this user may signal.
    if pid == 0 || i32::try_from(pid).is_err() {
        return Err(SignalError::NotASingleProcess { pid });
    }
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .map_err(|error| SignalError::NoSignalProgram(error.to_string()))?;
    if status.success() {
        return Ok(());
    }
    Err(SignalError::Refused { pid, signal })
}

#[cfg(test)]
mod tests {
    use super::{INTERRUPT, SignalError, send};

    #[test]
    fn a_pid_kill_would_read_as_a_group_or_as_everyone_is_refused_before_anything_is_sent() {
        for pid in [0, u32::MAX, 1 << 31] {
            let outcome = send(pid, INTERRUPT);
            assert!(
                matches!(outcome, Err(SignalError::NotASingleProcess { pid: refused }) if refused == pid),
                "pid {pid} reached the kill program: {outcome:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_pid_that_no_longer_exists_is_refused_rather_than_reported_as_sent() {
        // Linux never hands out a pid at or above pid_max, so this one names
        // no process and the kernel refuses it with ESRCH.
        let pid_max: u32 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .expect("pid_max is readable")
            .trim()
            .parse()
            .expect("pid_max is a number");
        assert!(matches!(
            send(pid_max, INTERRUPT),
            Err(SignalError::Refused { .. })
        ));
    }
}
