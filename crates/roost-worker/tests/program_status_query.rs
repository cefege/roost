//! The OSC 7501 feature query, answered by the worker's reply lane: a query
//! under BEL or ST is answered exactly once, a query split at any offset is
//! answered once, the capture lane answers nothing, and a program status
//! report (not a query) passes through without an answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{RioCore, TerminalCore};
use roost_worker::session::query_reply::{QueryReply, answer_queries};

const FEATURE_REPLY: &str =
    "\x1b]7501 ; ? : states=idle,working,done,blocked,error : kinds=permission,question,auth\x1b\\";

fn answer(core: &mut RioCore, carry: &mut Vec<u8>, chunk: &[u8]) -> QueryReply {
    answer_queries(carry, Some(core as &mut dyn TerminalCore), chunk)
}

fn answer_fresh(chunk: &[u8]) -> QueryReply {
    answer(&mut RioCore::new(80, 24), &mut Vec::new(), chunk)
}

#[test]
fn a_query_terminated_by_bel_is_answered_with_the_feature_reply() {
    assert_eq!(answer_fresh(b"\x1b]7501;?\x07").bytes, FEATURE_REPLY);
}

#[test]
fn a_query_terminated_by_st_is_answered_with_the_feature_reply() {
    assert_eq!(answer_fresh(b"\x1b]7501;?\x1b\\").bytes, FEATURE_REPLY);
}

#[test]
fn a_query_split_at_every_offset_is_answered_exactly_once() {
    let query: &[u8] = b"\x1b]7501;?\x1b\\";
    for cut in 1..query.len() {
        let mut core = RioCore::new(80, 24);
        let mut carry = Vec::new();
        let first = answer(&mut core, &mut carry, &query[..cut]);
        let second = answer(&mut core, &mut carry, &query[cut..]);
        let combined = format!("{}{}", first.bytes, second.bytes);
        assert_eq!(combined, FEATURE_REPLY, "cut at {cut}");
    }
}

#[test]
fn the_capture_lane_answers_no_query() {
    let mut carry = Vec::new();
    let reply = answer_queries(&mut carry, None, b"\x1b]7501;?\x07");
    assert!(reply.bytes.is_empty());
    assert_eq!(reply.synth_bytes, 0);
}

#[test]
fn a_program_status_report_is_not_a_query_and_is_answered_nothing() {
    let reply = answer_fresh(b"\x1b]7501;state=done:app=x\x1b\\");
    assert!(reply.bytes.is_empty());
    assert_eq!(reply.synth_bytes, 0);
}
