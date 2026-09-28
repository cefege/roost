//! Real PTYs through the keeper's dispatcher: every byte value reaches a raw
//! `cat` exactly once and in order, whichever frame carried it (the
//! "backspace acts like space" guard, v2 `apps/worker/tests/keeper-input-stress.test.ts`),
//! and a child that stops reading stalls only its own channel instead of the
//! keeper (v2 `keeper-input-queue.ts`: the write is off the dispatch path).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use roost_keeper::codec::{KEEPER_MAX_INPUT_BYTES, MuxFrame, MuxFrameType};
use roost_keeper::frames::ShellSpec;
use roost_keeper::input_queue::KEEPER_INPUT_QUEUE_MAX_COMMANDS;
use roost_keeper::keeper::Keeper;
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use support::{DEADLINE, drain_until, input_frame, next_result, spawn_with, tap_results};

/// `cat` behind a tty in raw mode: no echo, no translation, so any byte the
/// keeper substituted shows up as a difference.
fn raw_cat() -> ShellSpec {
    raw_shell("stty raw -echo; printf ready; exec cat")
}

fn raw_shell(script: &str) -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        cwd: None,
    }
}

fn legacy_frame(channel_id: u16, bytes: &[u8]) -> MuxFrame {
    MuxFrame::new(MuxFrameType::PtyIn, channel_id, bytes.to_vec()).expect("a small payload")
}

/// A raw cat on channel 1 whose tty is already raw: `ready` is printed only
/// after `stty raw` returned, so no input can meet the cooked tty (which v2
/// slept past) and nothing but `cat`'s echo follows it.
fn raw_channel(keeper: &mut Keeper) {
    keeper.handle(&spawn_with(1, raw_cat(), 200, 50));
    drain_until(keeper, |seen| {
        seen.windows(5).any(|window| window == b"ready")
    });
}

/// Every byte value, one frame each, alternating legacy and acknowledged
/// frames: exactly these bytes, in this order, and one answer per ack.
///
/// Sent in bursts of half the lane's command budget, each drained before the
/// next: 256 frames at once outrun a writer thread the scheduler is starving and
/// are refused at the 201st, which is the budget working. v2's stress test
/// stays under it the same way, awaiting each payload's echo (`sendAndExpect`).
#[test]
fn every_byte_value_crosses_both_frame_kinds_intact_and_in_order() {
    let mut keeper = Keeper::new();
    let results = tap_results(&mut keeper);
    raw_channel(&mut keeper);
    let expected: Vec<u8> = (0..=255).collect();
    let mut seen = Vec::new();
    for burst in expected.chunks(KEEPER_INPUT_QUEUE_MAX_COMMANDS / 2) {
        for byte in burst {
            let frame = if byte % 2 == 0 {
                legacy_frame(1, &[*byte])
            } else {
                input_frame(1, u64::from(*byte), &[*byte])
            };
            assert!(
                keeper.handle(&frame).is_empty(),
                "queued input is answered by the lane"
            );
        }
        seen.extend(drain_until(&mut keeper, |echo| echo.len() >= burst.len()));
    }
    assert_eq!(seen, expected);
    for byte in (1..=255u64).step_by(2) {
        let answer = next_result(&results);
        assert_eq!(
            PtyInResult::decode(answer.frame_type, &answer.payload),
            Some(PtyInResult::Ack {
                input_seq: byte,
                written: 1
            })
        );
    }
}

/// The original repro: a burst of single-byte DEL frames stays DEL.
#[test]
fn a_hundred_back_to_back_backspaces_stay_backspaces() {
    let mut keeper = Keeper::new();
    raw_channel(&mut keeper);
    for _ in 0..100 {
        keeper.handle(&legacy_frame(1, &[0x7f]));
    }
    let seen = drain_until(&mut keeper, |seen| seen.len() >= 100);
    assert_eq!(seen, vec![0x7f; 100]);
}

/// A paste burst and the multi-byte keys that ride beside it: arrows, SS3,
/// UTF-8, kill-line.
#[test]
fn a_paste_and_multibyte_keys_round_trip_exactly() {
    let mut keeper = Keeper::new();
    let results = tap_results(&mut keeper);
    raw_channel(&mut keeper);
    let paste: Vec<u8> = (0..4096u32)
        .map(|index| b'A' + (index % 26) as u8)
        .collect();
    let keys: &[&[u8]] = &[
        b"\x1b[A",
        b"\x1bOP",
        "🔥汉é".as_bytes(),
        &[0x15, b'h', 0x7f, 0x1b, b'[', b'D'],
    ];
    keeper.handle(&input_frame(1, 1, &paste));
    let mut expected = paste.clone();
    for key in keys {
        keeper.handle(&legacy_frame(1, key));
        expected.extend_from_slice(key);
    }
    let seen = drain_until(&mut keeper, |seen| seen.len() >= expected.len());
    assert_eq!(seen, expected);
    let answer = next_result(&results);
    assert_eq!(
        PtyInResult::decode(answer.frame_type, &answer.payload),
        Some(PtyInResult::Ack {
            input_seq: 1,
            written: 4096
        })
    );
}

/// A child that never reads its tty fills the PTY; the keeper keeps
/// dispatching, another channel keeps echoing, and the wedged channel is
/// refused at its byte budget, before anything more is written.
#[test]
fn a_child_that_stops_reading_stalls_only_its_own_channel() {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut keeper = Keeper::new();
        let results = tap_results(&mut keeper);
        raw_channel(&mut keeper);
        // Raw, so a full tty throttles its writer instead of discarding a line
        // it can never complete; `sleep` never reads, so the PTY fills.
        keeper.handle(&spawn_with(
            2,
            raw_shell("stty raw -echo; printf ready; exec sleep 30"),
            80,
            24,
        ));
        drain_until(&mut keeper, |seen| {
            seen.windows(5).any(|window| window == b"ready")
        });
        let batch = vec![b'x'; KEEPER_MAX_INPUT_BYTES as usize];
        let refused_at = (1..=32u64).find(|seq| {
            keeper
                .handle(&input_frame(2, *seq, &batch))
                .first()
                .is_some_and(|reply| {
                    PtyInResult::decode(reply.frame_type, &reply.payload)
                        == Some(PtyInResult::Reject {
                            input_seq: *seq,
                            reason: PtyInRejectReason::QueueFull,
                        })
                })
        });
        keeper.handle(&input_frame(1, 1, b"alive"));
        let echo = drain_until(&mut keeper, |seen| {
            seen.windows(5).any(|window| window == b"alive")
        });
        let alive_answered = std::iter::repeat_with(|| next_result(&results))
            .find(|frame| frame.channel_id == 1)
            .map(|frame| PtyInResult::decode(frame.frame_type, &frame.payload));
        keeper.handle(&MuxFrame::new(MuxFrameType::KillChild, 2, Vec::new()).expect("empty"));
        let _ = done.send((refused_at, echo, alive_answered));
    });
    let (refused_at, echo, alive_answered) = finished
        .recv_timeout(DEADLINE + Duration::from_secs(10))
        .expect("the keeper blocked on a PTY write instead of queueing it");
    assert!(
        refused_at.is_some(),
        "the wedged channel's budget refused a batch"
    );
    assert!(echo.windows(5).any(|window| window == b"alive"));
    assert_eq!(
        alive_answered,
        Some(Some(PtyInResult::Ack {
            input_seq: 1,
            written: 5
        }))
    );
}
