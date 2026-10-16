//! Model-facing text for a message from a peer conversation. History delivers it
//! as a user-role context message, the way a settled background job arrives, so
//! the recipient reads it as an unprompted arrival rather than its own reply.
//! Kept out of `history.rs`, which is at its size cap.

/// The `<system-notice>` message that reports one peer message to the model.
pub fn peer_message_text(from: &str, from_id: &str, broadcast: bool, text: &str) -> String {
    let scope = if broadcast {
        "a broadcast"
    } else {
        "a message"
    };
    format!(
        "<system-notice>\nYou received {scope} from {from} ({from_id}) while working. \
         Read it and reply with the message tool if it needs an answer.\n\n{text}\n</system-notice>"
    )
}

/// The compaction-summary line for a peer message.
pub fn peer_summary_text(from: &str, text: &str) -> String {
    format!("\n[message from {from}] {text}\n")
}
