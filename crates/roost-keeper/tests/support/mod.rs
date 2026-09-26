//! Helpers shared by the keeper's integration tests: frame builders and a
//! bounded drain. Every test that touches a real PTY uses these, so no test
//! depends on how fast a process got scheduled.
//!
//! `DEADLINE` is the point: a test that hangs is indistinguishable from a
//! keeper that does, so every wait here fails rather than blocks.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use roost_keeper::codec::MuxFrame;
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::{ShellSpec, SpawnRequest};
use roost_keeper::keeper::Keeper;
use roost_keeper::payloads::{PtyInRequest, ResizeRequest};

pub mod daemon;

pub const DEADLINE: Duration = Duration::from_secs(10);

/// A shell that echoes its input, so a round trip is observable.
pub fn echo() -> ShellSpec {
    ShellSpec {
        program: "/bin/cat".into(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
    }
}

/// A shell that runs one command and exits.
pub fn running(script: &str) -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: Vec::new(),
        cwd: None,
    }
}

/// A shell that stays alive, for tests about geometry rather than output.
pub fn idle() -> ShellSpec {
    ShellSpec {
        program: "/bin/sleep".into(),
        args: vec!["30".into()],
        env: Vec::new(),
        cwd: None,
    }
}

pub fn spawn_frame(channel_id: u16, cols: u16, rows: u16) -> MuxFrame {
    spawn_with(channel_id, echo(), cols, rows)
}

pub fn spawn_with(channel_id: u16, shell_spec: ShellSpec, cols: u16, rows: u16) -> MuxFrame {
    let request = SpawnRequest {
        channel_id,
        cols,
        rows,
        shell_spec,
    };
    MuxFrame::json(MuxFrameType::Spawn, channel_id, &request)
        .expect("a spawn request is small JSON")
}

pub fn input_frame(channel_id: u16, seq: u64, bytes: &[u8]) -> MuxFrame {
    let request = PtyInRequest {
        input_seq: seq,
        bytes: bytes.to_vec(),
    };
    MuxFrame::new(MuxFrameType::PtyInRequest, channel_id, request.encode())
        .expect("a small input payload is within every frame bound")
}

pub fn resize_frame(channel_id: u16, seq: u64, cols: u16, rows: u16) -> MuxFrame {
    let request = ResizeRequest { seq, cols, rows };
    MuxFrame::new(
        MuxFrameType::ResizeRequest,
        channel_id,
        request.encode().expect("a valid size"),
    )
    .expect("a resize payload is within every frame bound")
}

pub fn empty_frame(frame_type: MuxFrameType, channel_id: u16) -> MuxFrame {
    MuxFrame::new(frame_type, channel_id, Vec::new()).expect("an empty payload fits every frame")
}

/// Drain the keeper's output until `predicate` sees what it wants, returning
/// everything seen. Fails at the deadline rather than hanging.
pub fn drain_until(keeper: &mut Keeper, predicate: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let start = Instant::now();
    let mut seen: Vec<u8> = Vec::new();
    while start.elapsed() < DEADLINE {
        for frame in keeper.drain_output(8192) {
            seen.extend_from_slice(&frame.payload);
        }
        if predicate(&seen) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "never satisfied within {DEADLINE:?}; saw {:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// Wait for a condition that is not about output, such as a child exiting.
pub fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("{what} never happened within {DEADLINE:?}");
}
