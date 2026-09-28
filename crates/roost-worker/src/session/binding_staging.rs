//! The pre-record hold a `RecordBinding` keeps a channel's output in: what is
//! held, the staging buffer and its refusal bound, and the staged/live mode.
//! `session::binding` is the only user; it swaps a `Staged` mode to `Live` in
//! one critical section. Depends on `binding::RESUME_STAGE_CAP_BYTES` alone.

use super::binding::RESUME_STAGE_CAP_BYTES;

/// One thing a delivery is holding because the record is not there yet.
#[derive(Debug)]
pub(super) enum Held {
    Output(Vec<u8>),
    Exit(Option<i32>),
    Error(String),
}

/// Output that arrived before the record it belongs to.
#[derive(Debug, Default)]
pub(super) struct Staging {
    pub(super) events: Vec<Held>,
    pub(super) bytes: usize,
    pub(super) overflowed: bool,
}

impl Staging {
    /// Hold one chunk, or give up on the whole stream.
    ///
    /// Post-bound chunks are dropped WITHOUT buffering rather than trimmed to
    /// fit: the stream's integrity is already lost and the caller refuses the
    /// adoption the moment it looks, so keeping a partial tail would only make
    /// the hole look smaller than it is.
    pub(super) fn stage_output(&mut self, chunk: &[u8]) {
        if self.overflowed {
            return;
        }
        self.bytes += chunk.len();
        if self.bytes > RESUME_STAGE_CAP_BYTES {
            self.overflowed = true;
            self.events.clear();
            tracing::warn!(
                staged_bytes = self.bytes,
                cap_bytes = RESUME_STAGE_CAP_BYTES,
                "pty output outgrew the staging bound; this stream can no longer be adopted whole"
            );
            return;
        }
        self.events.push(Held::Output(chunk.to_vec()));
    }

    pub(super) fn stage_exit(&mut self, exit_code: Option<i32>) {
        if !self.overflowed {
            self.events.push(Held::Exit(exit_code));
        }
    }

    pub(super) fn stage_error(&mut self, reason: String) {
        if !self.overflowed {
            self.events.push(Held::Error(reason));
        }
    }
}

/// Whether a channel's delivery is still holding output for a record.
#[derive(Debug)]
pub(super) enum Mode {
    Staged(Staging),
    Live,
}
