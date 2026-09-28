//! The survivor-adoption replay as a parser-alignment problem. The keeper's
//! window opens wherever its own ring last evicted, so the first replayed byte
//! can be the tail of a sequence whose `ESC [` is gone; a cold parser prints
//! that remnant as literal text and nothing downstream ever re-parses it
//! (production: `32m1969M` burned into an htop grid). Ports
//! `apps/worker/tests/session/session-resume-replay-align.test.ts` and pins
//! `session::replay_align`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::Arc;

use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_worker::session::keeper_channels::SurvivorHistory;
use roost_worker::session::replay_align::{MAX_SEQ_LOOKBEHIND, skip_orphan_sequence_prefix};

use session_support::{Harness, SESSION, ScriptedKeeper, session_id};

const CHANNEL: u16 = 11;
/// The tail of `ESC [ 1 ; 32 m` left behind after eviction overwrote the lead.
const ORPHAN_TAIL: &str = "32m1969M";
/// What the TUI paints next: a cursor address, so it survives the prefix drop.
const REPAINT: &str = "\x1b[2;1HMEM-OK-REPAINT";

/// Adopt a survivor whose keeper reports `records` under `head_seq`. A head
/// past the records' sum is exactly how eviction is provable: it counts every
/// byte the PTY ever produced.
async fn adopt(records: Vec<&[u8]>, head_seq: u64, rows: u16) -> Harness {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(CHANNEL, 4242));
    *keeper.history.lock().expect("held") = SurvivorHistory {
        records: records
            .into_iter()
            .enumerate()
            .map(|(_, bytes)| HistoryRecord::Output {
                bytes: bytes.to_vec(),
            })
            .collect(),
        head_seq,
        base_cols: 80,
        base_rows: rows,
    };
    *keeper.applied.lock().expect("held") = TerminalState {
        applied_seq: 7,
        cols: 80,
        rows,
    };
    let harness = Harness::with_keeper(keeper);
    harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, CHANNEL, "/"))
        .await
        .expect("the survivor is adoptable");
    harness
}

/// History then viewport, one line per row, as the core holds it.
fn core_text(harness: &Harness) -> String {
    harness
        .table
        .with_record(&session_id(SESSION), |record| {
            let core = &record.terminal_core;
            let line = |cell: &dyn Fn(u16) -> u32, width: u16| -> String {
                (0..width)
                    .map(|col| {
                        char::from_u32(cell(col))
                            .filter(|c| *c != '\0')
                            .unwrap_or(' ')
                    })
                    .collect()
            };
            let mut lines = Vec::new();
            for offset in (0..core.scrollback_count()).rev() {
                let width = core.scrollback_line_len(offset) as u16;
                lines.push(line(
                    &|col| core.scrollback_cell(offset, col).character,
                    width,
                ));
            }
            for row in 0..core.rows() {
                lines.push(line(
                    &|col| core.viewport_cell(row, col).character,
                    core.cols(),
                ));
            }
            lines.join("\n")
        })
        .expect("the adopted session is live")
}

fn geometry(harness: &Harness) -> (u16, u16) {
    harness
        .table
        .with_record(&session_id(SESSION), |record| {
            (record.terminal_core.cols(), record.terminal_core.rows())
        })
        .expect("the adopted session is live")
}

#[tokio::test]
async fn an_evicted_window_opening_mid_sgr_replays_no_literal_text_and_keeps_the_repaint() {
    let bytes = format!("{ORPHAN_TAIL}{REPAINT}");
    let harness = adopt(vec![bytes.as_bytes()], bytes.len() as u64 + 4096, 24).await;
    let text = core_text(&harness);
    assert!(
        !text.contains("32m"),
        "the orphan remnant printed: {text:?}"
    );
    assert!(
        !text.contains("1969M"),
        "the orphan remnant printed: {text:?}"
    );
    assert!(text.contains("MEM-OK-REPAINT"));
    assert!(harness.keeper.resized.lock().expect("held").is_empty());
    assert_eq!(geometry(&harness), (80, 24));
}

/// The RING keeps the untrimmed bytes: every later boundary offset is derived
/// from `head_seq - retained`, and shortening one without the other skews it.
#[tokio::test]
async fn the_ring_keeps_the_untrimmed_bytes() {
    let bytes = format!("{ORPHAN_TAIL}{REPAINT}");
    let head_seq = bytes.len() as u64 + 4096;
    let harness = adopt(vec![bytes.as_bytes()], head_seq, 24).await;
    let (head, retained) = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            (record.head_seq, record.scrollback.len() as u64)
        })
        .expect("the adopted session is live");
    assert_eq!(retained, bytes.len() as u64);
    assert_eq!(head - retained, 4096);
}

/// A head equal to the retained bytes proves the window starts at the true
/// start of the stream: token-aligned by construction, nothing to drop.
#[tokio::test]
async fn an_unevicted_window_keeps_its_leading_plain_text() {
    let bytes = b"hello-unevicted\r\n";
    let harness = adopt(vec![bytes], bytes.len() as u64, 24).await;
    assert!(core_text(&harness).contains("hello-unevicted"));
    assert_eq!(geometry(&harness), (80, 24));
}

/// Only the cold core's FIRST write is trimmed: every later record continues
/// a now-warm parser and is replayed verbatim, even one with no ESC at all.
#[tokio::test]
async fn only_the_cold_first_write_is_trimmed() {
    let first = format!("{ORPHAN_TAIL}\x1b[2;1HFIRST");
    let second = b"-SECOND-VERBATIM";
    let head_seq = (first.len() + second.len()) as u64 + 4096;
    let harness = adopt(vec![first.as_bytes(), second], head_seq, 24).await;
    let text = core_text(&harness);
    assert!(
        !text.contains("32m"),
        "the orphan remnant printed: {text:?}"
    );
    assert!(text.contains("FIRST-SECOND-VERBATIM"));
}

#[tokio::test]
async fn an_evicted_one_row_terminal_rebuilds_at_its_original_geometry() {
    let bytes = b"one-row-evicted";
    let harness = adopt(vec![bytes], bytes.len() as u64 + 1, 1).await;
    assert!(harness.keeper.resized.lock().expect("held").is_empty());
    assert_eq!(geometry(&harness), (80, 1));
}

#[test]
fn leading_continuation_bytes_are_skipped() {
    assert_eq!(skip_orphan_sequence_prefix(&[0x80, 0xbf, 0xa0, b'a']), 3);
    assert_eq!(skip_orphan_sequence_prefix(&[0x80, 0x80]), 2);
    assert_eq!(skip_orphan_sequence_prefix(b""), 0);
}

/// An ESC past the lookbehind proves the cut landed in text, which is kept:
/// an unbounded scan would discard a whole ESC-free window (a build log).
#[test]
fn the_escape_scan_is_bounded() {
    let near = format!("{}\x1b[0m", "x".repeat(MAX_SEQ_LOOKBEHIND - 1));
    assert_eq!(
        skip_orphan_sequence_prefix(near.as_bytes()),
        MAX_SEQ_LOOKBEHIND - 1
    );
    let far = format!("{}\x1b[0m", "x".repeat(MAX_SEQ_LOOKBEHIND));
    assert_eq!(skip_orphan_sequence_prefix(far.as_bytes()), 0);
    assert_eq!(skip_orphan_sequence_prefix(b"no escape at all"), 0);
}
