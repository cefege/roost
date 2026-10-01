//! The tests for `voice::handshake`, split out so the module stays under the cap.

use super::*;

fn keyterms(count: usize, term: &str) -> Vec<(String, String)> {
    (0..count)
        .map(|index| (format!("{term}{index}"), format!("{term} {index}")))
        .collect()
}

fn keyterm_bytes(url: &str) -> usize {
    url.split("&keyterm=")
        .skip(1)
        .map(|term| 9 + term.len())
        .sum()
}

#[test]
fn the_listen_url_carries_the_model_and_the_linear16_encoding() {
    let url = build_url("en", &[]);
    assert!(url.starts_with("wss://api.deepgram.com/v1/listen?"));
    for parameter in [
        "model=nova-3",
        "encoding=linear16",
        "sample_rate=16000",
        "channels=1",
        "smart_format=true",
        "punctuate=true",
        "interim_results=true",
        "language=en",
    ] {
        assert!(url.contains(parameter), "{parameter} missing from {url}");
    }
}

#[test]
fn an_empty_language_falls_back_to_english() {
    assert!(build_url("", &[]).contains("language=en"));
    assert!(build_url("en-GB", &[]).contains("language=en-GB"));
    assert!(is_english("en-GB"));
    assert!(!is_english("multi"));
    assert!(!is_english("__auto__"));
    assert!(is_english(""));
}

#[test]
fn the_multilingual_and_auto_languages_ask_for_their_own_modes() {
    assert!(build_url("multi", &[]).contains("language=multi"));
    let auto = build_url("__auto__", &[]);
    assert!(auto.contains("detect_language=true"));
    assert!(!auto.contains("&language="), "auto must not pin a language");
}

#[test]
fn keyterms_are_injected_only_for_english() {
    let terms = keyterms(3, "kysely");
    let english = build_url("en", &terms);
    assert!(english.contains("&keyterm=kysely%200"));
    assert!(!build_url("multi", &terms).contains("keyterm"));
    assert!(!build_url("__auto__", &terms).contains("keyterm"));
}

#[test]
fn the_keyterm_budget_is_bounded_by_the_count() {
    let url = build_url("en", &keyterms(MAX_KEYTERM_COUNT + 25, "term"));
    assert_eq!(url.matches("&keyterm=").count(), MAX_KEYTERM_COUNT);
    assert!(url.len() <= MAX_WS_URL_LEN);
}

#[test]
fn the_keyterm_budget_is_bounded_by_the_bytes() {
    let fat = keyterms(MAX_KEYTERM_COUNT, "averyveryverylongkeytermvariantindeed");
    let url = build_url("en", &fat);
    let injected = url.matches("&keyterm=").count();
    assert!(
        injected < MAX_KEYTERM_COUNT,
        "the count bound should not have bound"
    );
    assert!(injected > 0, "no keyterm survived the byte budget");
    assert!(
        keyterm_bytes(&url) <= MAX_KEYTERM_URL_BUDGET,
        "terms cost {} of a {MAX_KEYTERM_URL_BUDGET} budget",
        keyterm_bytes(&url)
    );
    assert!(url.len() <= MAX_WS_URL_LEN);
}

#[test]
fn encoding_matches_encode_uri_component() {
    assert_eq!(uri_encode("Kysely Query"), "Kysely%20Query");
    assert_eq!(uri_encode("a-b_c.d!~*'()"), "a-b_c.d!~*'()");
    assert_eq!(uri_encode("x/y&z=1"), "x%2Fy%26z%3D1");
}

#[test]
fn only_an_asked_for_close_is_expected() {
    assert!(is_expected_close(1000, None));
    assert!(is_expected_close(1006, Some(CloseIntent::Send)));
    assert!(is_expected_close(1006, Some(CloseIntent::Cancel)));
    assert!(!is_expected_close(1006, None));
    assert!(!is_expected_close(4001, None));
}

#[test]
fn every_close_class_has_its_own_caption() {
    assert!(close_message(1006, "").contains("connection dropped (network)"));
    for code in [1011, 1012, 1013] {
        assert!(close_message(code, "").contains("server hiccup"));
    }
    assert!(close_message(4001, "invalid key").contains("(4001: invalid key)"));
    assert!(close_message(4001, "").contains("(4001)"));
    assert!(close_message(1015, "weird").contains("(1015: weird)"));
}

#[test]
fn a_blocked_mic_is_named_as_a_block_rather_than_a_busy_device() {
    let failure = mic_open_failure("NotAllowedError", "denied");
    assert!(failure.message.starts_with("Mic blocked for this site"));
    assert_eq!(failure.name, "NotAllowedError");
    assert_eq!(failure.detail, "denied");
    assert!(
        mic_open_failure("Error", "Permission denied")
            .message
            .starts_with("Mic blocked")
    );
}

#[test]
fn a_busy_device_is_told_how_to_free_it() {
    assert!(
        mic_open_failure("NotReadableError", "")
            .message
            .contains("another tab or app")
    );
    assert!(
        mic_open_failure("Error", "Could not start audio")
            .message
            .contains("busy")
    );
    assert!(mic_open_failure("AbortError", "").message.contains("busy"));
}

#[test]
fn a_missing_microphone_and_an_unknown_failure_are_distinct() {
    assert_eq!(
        mic_open_failure("NotFoundError", "").message,
        "No microphone found on this device."
    );
    assert_eq!(mic_open_failure("WeirdError", "odd").message, "Mic: odd");
    assert_eq!(
        mic_open_failure("WeirdError", "").message,
        "Mic: WeirdError"
    );
}

#[test]
fn the_captions_the_specs_assert_on_are_present_verbatim() {
    assert!(captions::SILENT.contains("sent no audio"));
    assert_eq!(
        captions::START_STALLED,
        "Mic didn't open — tap the mic again."
    );
    assert_eq!(
        captions::ATTACH_REFUSED,
        "Mic didn't start — tap the mic and try again."
    );
    assert!(captions::SERVICE_UNAVAILABLE.contains("Voice service unavailable"));
    assert_eq!(
        audio_session_stalled("suspended"),
        "audio session stayed suspended — tap the mic again"
    );
    assert_eq!(
        pipeline_timeout("the microphone"),
        "the microphone did not respond in time — tap the mic again"
    );
    assert_eq!(
        credential_rejected("INVALID_AUTH"),
        "Deepgram rejected the request: INVALID_AUTH"
    );
}

#[test]
fn a_pipeline_caption_is_not_prefixed_with_a_label() {
    let failure = message_failure(&pipeline_timeout("the audio worklet"));
    assert_eq!(
        failure.message,
        "the audio worklet did not respond in time — tap the mic again"
    );
    assert!(failure.name.is_empty());
}

#[test]
fn an_unconfigured_key_is_not_a_transport_failure() {
    assert!(credential_is_absent("Deepgram not configured"));
    assert!(credential_is_absent(
        "rpc error: code = FailedPrecondition desc = Deepgram not configured"
    ));
    assert!(!credential_is_absent("connection reset"));
}

#[test]
fn the_hit_rate_is_a_fraction_of_the_injected_vocabulary() {
    let terms = vec![
        ("kysely".to_owned(), "kysely".to_owned()),
        ("tailnet".to_owned(), "tail net".to_owned()),
    ];
    assert_eq!(keyterm_hit_rate(&terms, ""), 0.0);
    assert_eq!(keyterm_hit_rate(&[], "anything"), 0.0);
    let rate = keyterm_hit_rate(&terms, "the Kysely query ran");
    assert!((rate - 0.5).abs() < f64::EPSILON, "rate was {rate}");
}

#[test]
fn a_term_must_be_a_whole_word_to_count_as_a_hit() {
    let terms = vec![("tailnet".to_owned(), "tailnet".to_owned())];
    assert_eq!(keyterm_hit_rate(&terms, "the tailnet is down"), 1.0);
    assert_eq!(keyterm_hit_rate(&terms, "tailnetctl refused"), 0.0);
}

/// The caption each CLASS of start failure produces, and that no two classes
/// collapse onto one sentence.
///
/// A refused microphone reported as one that never answered sends the operator
/// to wait instead of to their permissions, so the classes have to stay apart.
/// The inputs are the browser's own words for each class — a name, a message, or
/// both — because Safari and Chrome disagree about which they send.
#[test]
fn each_start_failure_class_has_its_own_caption() {
    let refused = |name: &str, message: &str| {
        mic_open_outcome(&MicOpenOutcome::Refused {
            name: name.to_owned(),
            message: message.to_owned(),
        })
        .message
    };
    let classes = [
        ("blocked", refused("NotAllowedError", "denied")),
        ("busy", refused("NotReadableError", "")),
        ("missing", refused("NotFoundError", "")),
        ("unclassified", refused("WeirdError", "odd")),
        (
            "stalled",
            mic_open_outcome(&MicOpenOutcome::Stalled("the microphone")).message,
        ),
    ];
    for (index, (label, caption)) in classes.iter().enumerate() {
        assert!(
            !caption.is_empty(),
            "{label} names something the operator can act on"
        );
        for (other_label, other) in classes.iter().skip(index + 1) {
            assert!(
                caption != other,
                "the {label} and {other_label} failures share one caption: {caption:?}"
            );
        }
    }
}

#[test]
fn a_blocked_mic_reads_as_blocked_however_the_browser_names_it() {
    // Safari answers a bare Error whose message carries the reason; Chrome
    // answers a named DOMException. Both are the same operator problem.
    for (name, message) in [
        ("NotAllowedError", "denied"),
        ("Error", "Permission denied by the system"),
        ("", "permission"),
    ] {
        let caption = mic_open_outcome(&MicOpenOutcome::Refused {
            name: name.to_owned(),
            message: message.to_owned(),
        })
        .message;
        assert!(
            caption.contains("Mic blocked"),
            "{name}/{message} -> {caption:?}"
        );
    }
}

#[test]
fn a_refused_mic_says_it_was_blocked_and_never_that_it_timed_out() {
    let blocked = mic_open_outcome(&MicOpenOutcome::Refused {
        name: "NotAllowedError".to_owned(),
        message: "denied".to_owned(),
    })
    .message;
    assert!(blocked.contains("Mic blocked"), "got {blocked:?}");
    assert!(!blocked.contains("did not respond in time"));
    assert!(!blocked.contains("didn't open"));
}

#[test]
fn a_stalled_open_names_the_step_that_was_waited_on() {
    for step in ["the microphone", "the audio worklet"] {
        let stalled = mic_open_outcome(&MicOpenOutcome::Stalled(step)).message;
        assert!(stalled.contains(step), "got {stalled:?}");
        assert!(stalled.contains("did not respond in time"));
        // A stall is not a refusal, a busy device or a missing microphone.
        assert!(!stalled.contains("Mic blocked"));
        assert!(!stalled.contains("busy"));
        assert!(!stalled.contains("No microphone"));
    }
}

#[test]
fn a_stalled_open_carries_no_browser_name_to_report() {
    let stalled = mic_open_outcome(&MicOpenOutcome::Stalled("the microphone"));
    assert!(stalled.name.is_empty());
    assert_eq!(
        stalled.detail,
        "the microphone did not respond in time — tap the mic again"
    );
}
