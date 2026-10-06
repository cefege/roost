//! One Deepgram frame, read as a decision with no socket in it.
//!
//! Split out of `super::deepgram_engine` because the wire format is where a
//! transcription path most easily goes quietly wrong: a frame that parses as
//! JSON but carries no alternatives, a rejection whose detail field moved, the
//! summary that is a closed stream's last word. All of it is decided here, over
//! the parsed message, where a test can pin it.
//! Ports the `ws.onmessage` half of `apps/web/src/voice/deepgramDictation.ts`.

use serde_json::Value;

/// What one inbound frame means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Not a frame this recording acts on: a speech or utterance event, or a
    /// result with nothing in it.
    Ignored,
    /// Words: finalized ones, and the hypothesis that may replace them.
    Transcript {
        /// The transcript the recognizer heard.
        transcript: String,
        /// Whether the engine will not revise these words.
        is_final: bool,
    },
    /// The summary a stream sends after its last result, as the service closes
    /// it: every word for the audio it was sent has been delivered.
    StreamEnded,
    /// The service refused the session. The detail is what the operator is told.
    Rejected(String),
}

/// Read one frame's text.
///
/// A payload that is not a JSON string is not a frame this recording can act
/// on, so it answers `Ignored` rather than guessing.
#[must_use]
pub fn read(text: &str) -> Frame {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Frame::Ignored;
    };
    read_message(&message)
}

/// Read one already-parsed frame.
#[must_use]
pub fn read_message(message: &Value) -> Frame {
    let kind = message
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind == "Error" || message.get("err_msg").is_some() {
        // The first key that PRESENT is not the first key that SAYS anything:
        // Deepgram sends an empty `err_msg` beside the real description, and a
        // caption that read "" would leave the operator with no reason at all.
        let detail = ["err_msg", "description", "message"]
            .iter()
            .filter_map(|key| message.get(key).and_then(Value::as_str))
            .find(|value| !value.trim().is_empty())
            .unwrap_or("unknown");
        return Frame::Rejected(detail.to_owned());
    }
    if kind == "Metadata" {
        return Frame::StreamEnded;
    }
    if kind != "Results" {
        return Frame::Ignored;
    }
    let transcript = message
        .get("channel")
        .and_then(|channel| channel.get("alternatives"))
        .and_then(Value::as_array)
        .and_then(|alternatives| alternatives.first())
        .and_then(|alternative| alternative.get("transcript"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if transcript.is_empty() {
        return Frame::Ignored;
    }
    Frame::Transcript {
        transcript,
        is_final: message
            .get("is_final")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::Frame;
    use super::read;

    #[test]
    fn an_interim_result_carries_its_hypothesis_and_is_not_final() {
        let frame = read(
            r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"PCM ready","confidence":0.9}]}}"#,
        );
        assert_eq!(
            frame,
            Frame::Transcript {
                transcript: "PCM ready".to_owned(),
                is_final: false,
            }
        );
    }

    #[test]
    fn a_final_result_says_so() {
        let frame = read(
            r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"hello from the mic"}]}}"#,
        );
        assert_eq!(
            frame,
            Frame::Transcript {
                transcript: "hello from the mic".to_owned(),
                is_final: true,
            }
        );
    }

    #[test]
    fn the_summary_after_a_close_stream_ends_the_stream() {
        // The one answer a `CloseStream` always gets, after its last result.
        assert_eq!(
            read(r#"{"type":"Metadata","request_id":"abc","duration":2.5}"#),
            Frame::StreamEnded
        );
    }

    #[test]
    fn a_blank_transcript_is_ignored_rather_than_painting_nothing() {
        assert_eq!(
            read(
                r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"  "}]}}"#
            ),
            Frame::Ignored
        );
        assert_eq!(read(r#"{"type":"Results"}"#), Frame::Ignored);
    }

    #[test]
    fn a_rejection_carries_the_detail_the_operator_is_shown() {
        assert_eq!(
            read(r#"{"type":"Error","err_code":"INVALID_AUTH","err_msg":"invalid credentials"}"#),
            Frame::Rejected("invalid credentials".to_owned())
        );
        // An error frame with the detail under a different key still resolves:
        // the caption must never read "unknown" when the frame named a reason.
        assert_eq!(
            read(r#"{"err_msg":"","description":"quota exceeded"}"#),
            Frame::Rejected("quota exceeded".to_owned())
        );
        // An error code with neither the type nor the message field is not a
        // rejection this recording acts on: v2 keys on `type === "Error"` or a
        // present `err_msg`, and inventing a third trigger here would caption a
        // frame the service never refused.
        assert_eq!(read(r#"{"err_code":"X"}"#), Frame::Ignored);
    }

    #[test]
    fn a_broken_payload_is_not_a_failure() {
        assert_eq!(read(r#"{"type":"SpeechStarted"}"#), Frame::Ignored);
        assert_eq!(read("not json"), Frame::Ignored);
        assert_eq!(read(""), Frame::Ignored);
    }
}
