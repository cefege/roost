//! The Deepgram handshake: the URL one socket is opened with, and the words the
//! operator is shown when it fails.
//!
//! Split from the socket because both are decisions with no socket in them. The
//! URL is bounded — a long terminal buffer must not produce a URL a browser
//! refuses to open — and every failure caption is a string the Playwright
//! oracles assert on, so they live where a test can read them.
//! Ports `apps/web/src/voice/deepgramDictation.url.ts` and
//! `apps/web/src/voice/deepgramDictation.helpers.ts`.

/// The WebSocket endpoint. The browser talks to Deepgram directly with a key the
/// coordinator stored; there is no server-side transcription to route through.
pub const LISTEN_ENDPOINT: &str = "wss://api.deepgram.com/v1/listen";

/// How many keyterms ride one connection. Deepgram fixes the term list when the
/// socket opens, so this is a per-recording budget and not a per-frame one.
pub const MAX_KEYTERM_COUNT: usize = 50;

/// The byte budget for `&keyterm=` plus the encoded variant. The terms are
/// ranked, so this spends the budget on the best-scoring ones and drops the
/// tail.
pub const MAX_KEYTERM_URL_BUDGET: usize = 1500;

/// A last-resort assertion on the finished URL.
pub const MAX_WS_URL_LEN: usize = 8000;

/// The coordinator's "let Deepgram detect the language" setting.
pub const LANGUAGE_AUTO: &str = "__auto__";

/// Deepgram's multilingual English model.
pub const LANGUAGE_MULTI: &str = "multi";

/// Whether the stored language is English, which is the only case where a
/// keyterm variant can be spoken. Keyterms are an English-model feature.
#[must_use]
pub fn is_english(language: &str) -> bool {
    let language = if language.is_empty() { "en" } else { language };
    language != LANGUAGE_AUTO && language != LANGUAGE_MULTI && language.starts_with("en")
}

/// The fixed query parameters every listen URL carries.
fn base_query() -> Vec<(&'static str, &'static str)> {
    vec![
        ("model", "nova-3"),
        ("encoding", "linear16"),
        ("sample_rate", "16000"),
        ("channels", "1"),
        ("smart_format", "true"),
        ("punctuate", "true"),
        ("interim_results", "true"),
    ]
}

/// Build the listen URL for one recording.
///
/// Keyterms are appended in rank order and stop at whichever bound binds first,
/// the count or the bytes. Terms are dropped from the tail rather than refused:
/// the URL is what carries the bias, and a missing rare term costs less than a
/// socket that never opens.
#[must_use]
pub fn build_url(language: &str, keyterms: &[(String, String)]) -> String {
    let language = if language.is_empty() { "en" } else { language };
    let mut params = base_query();
    if language == LANGUAGE_AUTO {
        params.push(("detect_language", "true"));
    } else if language == LANGUAGE_MULTI {
        params.push(("language", LANGUAGE_MULTI));
    } else {
        params.push(("language", language));
    }
    let english = is_english(language);
    let mut accepted: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    if english {
        for (_term, variant) in keyterms {
            if accepted.len() >= MAX_KEYTERM_COUNT {
                break;
            }
            let cost = 9 + uri_encode(variant).len();
            if bytes + cost > MAX_KEYTERM_URL_BUDGET {
                break;
            }
            bytes += cost;
            accepted.push(variant);
        }
    }
    let mut query = url_encode_pairs(&params);
    for variant in accepted {
        query.push_str("&keyterm=");
        query.push_str(&uri_encode(variant));
    }
    let mut url = format!("{LISTEN_ENDPOINT}?{query}");
    // Unreachable while the budget holds; a websocket constructor's own limit is
    // not something to discover from the browser.
    while url.len() > MAX_WS_URL_LEN && url.contains("&keyterm=") {
        url = url
            .rsplit_once("&keyterm=")
            .map_or(url.clone(), |(head, _)| head.to_owned());
    }
    url
}

/// The `encodeURIComponent` of a JavaScript string: everything outside the
/// unreserved set becomes uppercase percent escapes, and a space is `%20`.
#[must_use]
pub fn uri_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let character = *byte;
        let unreserved = character.is_ascii_alphanumeric()
            || matches!(
                character,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if unreserved {
            encoded.push(char::from(character));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{character:02X}"));
        }
    }
    encoded
}

fn url_encode_pairs(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", uri_encode(key), uri_encode(value)))
        .collect::<Vec<String>>()
        .join("&")
}

/// Why a recording is being given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseIntent {
    /// The operator stopped it and wants the words.
    Send,
    /// The operator discarded it.
    Cancel,
    /// Nobody said.
    None,
}

/// Whether a socket close was the one this recording asked for.
///
/// Only an explicit end intent makes any code expected; a 1000 with no intent is
/// a server that hung up on its own, which is a failure to report.
#[must_use]
pub fn is_expected_close(code: u16, intent: Option<CloseIntent>) -> bool {
    code == 1000 || matches!(intent, Some(CloseIntent::Send | CloseIntent::Cancel))
}

/// The caption for a socket that closed for a reason nobody asked for.
#[must_use]
pub fn close_message(code: u16, reason: &str) -> String {
    let detail = if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    };
    match code {
        1006 => {
            "Deepgram connection dropped (network) — retried and still failed. Check your internet."
                .to_owned()
        }
        1011..=1013 => "Deepgram had a server hiccup — try again in a moment.".to_owned(),
        code if code >= 4000 => format!(
            "Deepgram rejected the session ({code}{detail}) — the API key may be invalid or \
             rate-limited (Settings → Voice)."
        ),
        code => format!("Deepgram closed unexpectedly ({code}{detail})."),
    }
}

/// Why the microphone would not open, in the operator's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicOpenFailure {
    /// The DOMException name, for the diagnostics event.
    pub name: String,
    /// The raw message, for the diagnostics event.
    pub detail: String,
    /// What the caption says.
    pub message: String,
}

/// Classify a rejected `getUserMedia`.
///
/// The name is not trusted on its own: Safari reports a bare `Error` whose
/// message carries the reason, and Chrome's names are not universal, so the
/// message is matched too.
#[must_use]
pub fn mic_open_failure(name: &str, message: &str) -> MicOpenFailure {
    let lowered = message.to_ascii_lowercase();
    let text = if name == "NotAllowedError" || mentions_any(&lowered, &["denied", "permission"]) {
        "Mic blocked for this site — allow it (address-bar icon → Microphone → Allow), then \
         reload."
    } else if name == "NotReadableError"
        || name == "AbortError"
        || mentions_any(
            &lowered,
            &["in use", "busy", "could not start", "failed to allocate"],
        )
    {
        "Mic is busy — another tab or app (WhatsApp, Telegram, Zoom…) is using it. Close it, \
         then retry."
    } else if name == "NotFoundError" {
        "No microphone found on this device."
    } else {
        return MicOpenFailure {
            name: name.to_owned(),
            detail: message.to_owned(),
            message: format!("Mic: {}", if message.is_empty() { name } else { message }),
        };
    };
    MicOpenFailure {
        name: name.to_owned(),
        detail: message.to_owned(),
        message: text.to_owned(),
    }
}

/// A failure whose caption is its own message, with no `Mic: ` prefix.
///
/// The open-pipeline captions are sentences an operator reads, not a
/// DOMException name and a detail; prefixing one would put a label in front of
/// the text a spec matches on.
#[must_use]
pub fn message_failure(message: &str) -> MicOpenFailure {
    MicOpenFailure {
        name: String::new(),
        detail: message.to_owned(),
        message: message.to_owned(),
    }
}

/// How a device open ended when it produced no device.
///
/// A refused open and a stalled one are different operator problems with
/// different fixes, and the browser tells them apart in the answer it gives:
/// a refusal arrives as the promise's own rejection naming its cause, while a
/// stall is the absence of any answer at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicOpenOutcome {
    /// The browser refused, with the rejection's own name and message.
    Refused {
        /// The `DOMException` name.
        name: String,
        /// The raw message.
        message: String,
    },
    /// The open did not finish inside the start grace.
    Stalled(&'static str),
}

/// The caption and diagnostics for an open that produced no device.
///
/// The one place a start failure becomes a sentence, so a refusal can never
/// fall through to the stall caption and report "did not respond in time" for a
/// microphone the browser had already refused.
#[must_use]
pub fn mic_open_outcome(outcome: &MicOpenOutcome) -> MicOpenFailure {
    match outcome {
        MicOpenOutcome::Refused { name, message } => mic_open_failure(name, message),
        MicOpenOutcome::Stalled(step) => message_failure(&pipeline_timeout(step)),
    }
}

fn mentions_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// The caption when the audio session never left `suspended`.
#[must_use]
pub fn audio_session_stalled(state: &str) -> String {
    format!("audio session stayed {state} — tap the mic again")
}

/// The caption when a step of the mic pipeline did not answer in time.
#[must_use]
pub fn pipeline_timeout(step: &str) -> String {
    format!("{step} did not respond in time — tap the mic again")
}

/// The captions the capture and transport halves own. Each is a string a spec
/// asserts on, so they are named here rather than built at a call site.
pub mod captions {
    /// A mic that opened but produced no frames, after one rebuild.
    pub const SILENT: &str = "Mic opened but sent no audio — tap the mic again (iOS only starts \
                              audio inside a fresh tap). If it repeats, close any other app using \
                              the mic.";
    /// The device open never finished inside the start grace.
    pub const START_STALLED: &str = "Mic didn't open — tap the mic again.";
    /// The capture attach reported that it did not attach.
    pub const ATTACH_REFUSED: &str = "Mic didn't start — tap the mic and try again.";
    /// The credential request failed twice.
    pub const SERVICE_UNAVAILABLE: &str = "Voice service unavailable — couldn't reach Deepgram \
                                          (coordinator may be restarting). Try again.";
}

/// The caption for a rejected Deepgram frame.
#[must_use]
pub fn credential_rejected(detail: &str) -> String {
    format!("Deepgram rejected the request: {detail}")
}

/// How much of the injected vocabulary the transcript actually used.
///
/// Reported as a diagnostics number, so a biasing change can be A/B'd instead of
/// argued about: a run where the hit rate collapses is a run where the keyterms
/// are costing bytes for nothing.
#[must_use]
pub fn keyterm_hit_rate(keyterms: &[(String, String)], transcript: &str) -> f64 {
    if keyterms.is_empty() {
        return 0.0;
    }
    let haystack = format!(" {} ", transcript.to_lowercase());
    let hits = keyterms
        .iter()
        .filter(|(term, _)| haystack.contains(&format!(" {} ", term.to_lowercase())))
        .count();
    hits as f64 / keyterms.len() as f64
}

/// Whether a rejection from the coordinator means "not configured" rather than
/// "broken": the operator's next move is a key, not a retry.
#[must_use]
pub fn credential_is_absent(error: &str) -> bool {
    let lowered = error.to_lowercase();
    lowered.contains("not configured") || lowered.contains("failed precondition")
}

#[cfg(test)]
mod tests;
