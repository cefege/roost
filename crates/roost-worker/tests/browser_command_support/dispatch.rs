// A fake that cannot say what it expected is not a fake. `expect` is denied
// outside `#[cfg(test)]`, and an integration-test module is its own crate,
// so the exemption is stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Turning a canonical frame into a command, and a dispatch into the one
//! reply it produced. The assertion helpers live here so every test binary
//! holds the dispatch to the same "exactly one, correlated" contract.

use serde_json::Value;

use super::{FINGERPRINT, every_kind};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_worker::browser_commands::Reply;
use roost_worker::browser_commands::scrollback_page::{GridDescription, history_floor_for};
use roost_worker::browser_commands::{Command, Deps};

/// The one reply a dispatch produced, insisting there was exactly one.
#[track_caller]
pub fn only(frames: Vec<CoordWorkerUpstream>) -> Reply {
    let mut frames = frames;
    assert_eq!(
        frames.len(),
        1,
        "a command that asks for an answer gets one"
    );
    match frames.remove(0) {
        CoordWorkerUpstream::RpcOk {
            request_id, data, ..
        } => {
            assert_eq!(request_id, "req-1", "the reply echoes the envelope id");
            Reply::ok(&request_id, data)
        }
        CoordWorkerUpstream::RpcError {
            request_id,
            message,
            ..
        } => {
            assert_eq!(request_id, "req-1", "a refusal is correlated too");
            Reply::error(&request_id, message)
        }
        other => panic!("a command answers with an rpc frame, not {other:?}"),
    }
}

pub fn command(value: Value) -> Command {
    Command::decode(FINGERPRINT, FINGERPRINT, "req-1", value).expect("a canonical frame decodes")
}

pub fn frame_of(kind: &str) -> Value {
    every_kind()
        .into_iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("{kind} is a canonical frame"))
}

/// The dispatch under test, for one command.
pub async fn dispatch(command: &Command, deps: &Deps) -> Vec<CoordWorkerUpstream> {
    roost_worker::browser_commands::dispatch(command, deps).await
}

/// The floor a page clamped at this window reports, as the wire spells it.
pub fn floor(description: &GridDescription, wanted_start: u32) -> String {
    history_floor_for(description, wanted_start)
        .as_wire()
        .to_owned()
}

pub fn base64_decode(value: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .expect("the reply is base64")
}
