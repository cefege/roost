//! One live PTY: the process, its master end, and the geometry and sequence
//! state the keeper answers questions about.
//!
//! Owned by the keeper. The worker never touches a PTY, so this is the only
//! place a channel exists.
//!
//! `portable-pty` does the forkpty and the controlling-TTY handshake, so this
//! module needs no raw descriptors of its own. The crate keeps its `unsafe`
//! allowance for the socket, not for the terminal.

use std::io::Write;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};

use crate::frames::ShellSpec;
use crate::output_ring::{OutputRing, spawn_reader};
use crate::payloads::{PtyInRejectReason, TerminalState};

/// Why a PTY could not be opened. Each variant is something the worker can act
/// on differently, so they are not collapsed into one opaque error.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("channel {0} is the control lane, which is never a PTY")]
    BadChannelId(u16),
    #[error("the {name} of {actual} is outside 1..={max}")]
    BadDimension {
        name: &'static str,
        actual: u16,
        max: u16,
    },
    #[error("the working directory {path} is not usable: {reason}")]
    BadCwd { path: String, reason: String },
    #[error("the PTY could not be opened: {0}")]
    Pty(String),
}

/// Why a write did not fully succeed. The distinction between `Rejected` and
/// `Partial` is the whole point: a retry is safe in the first case and
/// duplicates a character in the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Every byte reached the PTY.
    Complete { written: u32 },
    /// Nothing reached it, so a retry cannot duplicate.
    Rejected { reason: PtyInRejectReason },
    /// Some bytes reached it, so a retry WOULD duplicate.
    Partial {
        written: u32,
        reason: PtyInRejectReason,
    },
}

/// A live PTY and the state the keeper tracks for it.
pub struct PtyChannel {
    channel_id: u16,
    master: Box<dyn MasterPty + Send>,
    output: OutputRing,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// The last resize sequence the keeper actually applied. Zero means none,
    /// so the first applied sequence is 1 and a client can tell "no resize yet"
    /// from "resize to the default size".
    applied_seq: u64,
    cols: u16,
    rows: u16,
    /// Joining the reader on drop is what stops a channel's thread outliving
    /// it and holding a descriptor open on a PTY nobody is reading.
    _reader_thread: std::thread::JoinHandle<()>,
}

impl PtyChannel {
    /// Open a PTY running `spec` at this geometry.
    ///
    /// Channel 0 is the control lane on the wire and is never a PTY, so it is
    /// rejected here rather than being a channel the socket cannot address.
    pub fn spawn(
        channel_id: u16,
        spec: &ShellSpec,
        cols: u16,
        rows: u16,
    ) -> Result<Self, SpawnError> {
        if channel_id == 0 {
            return Err(SpawnError::BadChannelId(channel_id));
        }
        // Reject a zero or absurd geometry rather than clamping: a clamp
        // produces a PTY whose size differs from what the client was told, and
        // the client has no way to discover that.
        let max = crate::codec::KEEPER_MAX_TERMINAL_DIMENSION as u16;
        if cols == 0 || cols > max {
            return Err(SpawnError::BadDimension {
                name: "column count",
                actual: cols,
                max,
            });
        }
        if rows == 0 || rows > max {
            return Err(SpawnError::BadDimension {
                name: "row count",
                actual: rows,
                max,
            });
        }

        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| SpawnError::Pty(err.to_string()))?;

        let mut command = CommandBuilder::new(&spec.program);
        // The child gets the spec's environment and NOTHING else.
        //
        // `CommandBuilder` wraps `std::process::Command`, which INHERITS the
        // spawning process's environment, so without this call every PTY on
        // the machine inherits the keeper's own — including
        // `ROOST_KEEPER_CAPABILITY` and its siblings, which is exactly what
        // `keeperEndpointFromArgument` reads to talk to this keeper. A PTY
        // that inherits the control credential hands every command the user
        // runs the ability to speak to the keeper as this worker, which means
        // every terminal on the machine.
        //
        // v2 did not have this problem, and the reason is the point: `Bun.spawn`
        // treats its `env` option as a REPLACE, and v2's shell spec built that
        // value deliberately — a curated set read out of the worker's own
        // service environment plus an overlay (`shell-spec.ts`'s `environment`
        // and `envOverlay`). So clearing here is exact v2 parity, not a
        // tightening: the keeper is a faithful executor that applies what it
        // was told, and the decision about which variables a login shell
        // needs belongs in one place — the worker's `resolve_shell_spec`,
        // which is where v2 had it.
        command.env_clear();
        for arg in &spec.args {
            command.arg(arg);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        if let Some(cwd) = &spec.cwd {
            // Checked before the fork: a fork whose chdir then fails leaves a
            // child to reap with no way to know what it was doing.
            std::fs::metadata(cwd).map_err(|err| SpawnError::BadCwd {
                path: cwd.clone(),
                reason: err.to_string(),
            })?;
            command.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| SpawnError::Pty(err.to_string()))?;
        // The slave end is dropped here. Holding it open is what makes a
        // program see EOF never arrive, because the master still holds a
        // reference on the other side of the pty.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| SpawnError::Pty(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| SpawnError::Pty(err.to_string()))?;
        let (output, reader_thread) =
            spawn_reader(channel_id, reader).map_err(|err| SpawnError::Pty(err.to_string()))?;

        Ok(Self {
            channel_id,
            master: pair.master,
            output,
            writer,
            child,
            applied_seq: 0,
            cols,
            rows,
            _reader_thread: reader_thread,
        })
    }

    pub fn channel_id(&self) -> u16 {
        self.channel_id
    }

    /// The child's process id, which the worker records so an operator can
    /// find the process a channel is attached to.
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Apply a sequenced geometry change.
    ///
    /// A sequence at or below the applied one is ignored rather than
    /// rejected: the keeper is the ordering authority, and a client that
    /// retries after a lost ack must not move the terminal backwards.
    pub fn apply_resize(&mut self, seq: u64, cols: u16, rows: u16) -> Result<u64, SpawnError> {
        if seq <= self.applied_seq {
            return Ok(self.applied_seq);
        }
        if cols == 0 || rows == 0 {
            return Err(SpawnError::BadDimension {
                name: "geometry",
                actual: cols.max(rows),
                max: u16::MAX,
            });
        }
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| SpawnError::Pty(err.to_string()))?;
        self.applied_seq = seq;
        self.cols = cols;
        self.rows = rows;
        Ok(self.applied_seq)
    }

    /// The geometry the KERNEL has, which is not always the geometry this
    /// struct recorded. A resize that failed to reach the tty leaves the two
    /// disagreeing, and that disagreement is the only way to find out.
    pub fn master_size(&self) -> Result<PtySize, SpawnError> {
        self.master
            .get_size()
            .map_err(|err| SpawnError::Pty(err.to_string()))
    }

    /// The authoritative geometry, for a worker that lost a `ResizeAck` and
    /// has no retained marker left to ask about.
    pub fn terminal_state(&self) -> TerminalState {
        TerminalState {
            applied_seq: self.applied_seq,
            cols: self.cols,
            rows: self.rows,
        }
    }

    /// Write input, reporting exactly how much reached the PTY.
    ///
    /// A short write is reported as `Partial` rather than being completed
    /// here, because the bytes that DID land are indistinguishable from the
    /// ones still queued. Silently completing the write would duplicate them
    /// at the far end.
    pub fn write_input(&mut self, bytes: &[u8]) -> WriteOutcome {
        if bytes.is_empty() {
            return WriteOutcome::Complete { written: 0 };
        }
        if self.exited().is_some() {
            return WriteOutcome::Rejected {
                reason: PtyInRejectReason::ChildExited,
            };
        }
        match self.writer.write(bytes) {
            Ok(written) if written == bytes.len() => {
                // A buffered writer that accepted bytes is not proof they
                // reached the child, so the flush is checked and its failure
                // reported rather than swallowed.
                match self.writer.flush() {
                    Ok(()) => WriteOutcome::Complete {
                        written: written as u32,
                    },
                    Err(_) => WriteOutcome::Partial {
                        written: written as u32,
                        reason: PtyInRejectReason::PartialWrite,
                    },
                }
            }
            Ok(written) => WriteOutcome::Partial {
                written: written as u32,
                reason: PtyInRejectReason::PartialWrite,
            },
            Err(_) => WriteOutcome::Rejected {
                reason: PtyInRejectReason::NoReader,
            },
        }
    }

    /// Take whatever output is available, up to `limit` bytes.
    ///
    /// Returns `None` when nothing is ready, which is NOT the same as end of
    /// output — a PTY read that returned zero before the child exited would be
    /// indistinguishable from a closed channel. Ask
    /// [`PtyChannel::output_closed`] for that.
    pub fn read_output(&mut self, limit: usize) -> Option<Vec<u8>> {
        self.output.take(limit)
    }

    /// True once the child closed the slave end and everything it wrote has
    /// been handed over. This is the keeper's EOF signal, not a read returning
    /// zero bytes.
    pub fn output_closed(&mut self) -> bool {
        self.output.is_eof()
    }

    /// The child's exit status, or `None` while it is still running.
    pub fn exited(&mut self) -> Option<portable_pty::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Terminate the child. Used by `KillChild` and by shutdown.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl std::fmt::Debug for PtyChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyChannel")
            .field("channel_id", &self.channel_id)
            .field("pid", &self.child.process_id())
            .field("applied_seq", &self.applied_seq)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("output_drained", &self.output.is_drained())
            .finish()
    }
}
