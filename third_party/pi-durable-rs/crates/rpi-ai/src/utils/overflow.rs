//! Port of `packages/ai/src/utils/overflow.ts` @ pi 0.84.1+ (4181f66).
//!
//! Three-branch context-overflow detection: error-text pattern table (with a
//! non-overflow exclusion table), z.ai explicit/silent overflow, Xiaomi
//! truncation;
//! plus `isRecoverableLength` (32850ef7c).

use std::sync::LazyLock;

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

fn build_pattern(pattern: &str) -> Regex {
    // invariant: these are pinned literal patterns ported from overflow.ts;
    // they compile (verified by the tests below).
    Regex::new(pattern).expect("static overflow pattern must compile")
}

/// `OVERFLOW_PATTERNS` — order preserved from upstream (comments there name
/// the provider each pattern belongs to; the first pattern covers Anthropic
/// and z.ai per #9805).
fn overflow_patterns() -> &'static [Regex] {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            // #9805 (0e283203c): generalized to cover z.ai's explicit
            // `Prompt too long` (code 1261) alongside Anthropic's
            // `prompt is too long`.
            r"(?i)prompt (?:is )?too long",
            // #10208 (`3dd803d7e`): the z.ai CN endpoint reports
            // `Prompt exceeds max length` (code 1261).
            r"(?i)prompt exceeds max length",
            r"(?i)request_too_large",
            r"(?i)input is too long for requested model",
            r"(?i)exceeds the context window",
            r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
            r"(?i)input token count.*exceeds the maximum",
            r"(?i)maximum prompt length is \d+",
            r"(?i)reduce the length of the messages",
            r"(?i)maximum context length is \d+ tokens",
            r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
            r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)",
            r"(?i)exceeds the limit of \d+",
            r"(?i)exceeds the available context size",
            r"(?i)greater than the context length",
            r"(?i)context window exceeds limit",
            r"(?i)exceeded model token limit",
            r"(?i)too large for model with \d+ maximum context length",
            r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
            r"(?i)model_context_window_exceeded",
            r"(?i)prompt too long; exceeded (?:max )?context length",
            r"(?i)range of input length should be",
            r"(?i)context[_ ]length[_ ]exceeded",
            r"(?i)too many tokens",
            r"(?i)token limit exceeded",
        ]
        .iter()
        .map(|pattern| build_pattern(pattern))
        .collect()
    });
    &PATTERNS
}

/// `CEREBRAS_BODYLESS_OVERFLOW_PATTERN` (#9482, 661619e87 @ d1230ea20):
/// bodyless 400/413 responses are only an overflow signal from Cerebras —
/// other providers report genuine bad requests / payload limits that way.
fn cerebras_bodyless_overflow_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| build_pattern(r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)"));
    &PATTERN
}

/// `NON_OVERFLOW_PATTERNS`.
fn non_overflow_patterns() -> &'static [Regex] {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            r"(?i)^(Throttling error|Service unavailable):",
            r"(?i)rate limit",
            r"(?i)too many requests",
        ]
        .iter()
        .map(|pattern| build_pattern(pattern))
        .collect()
    });
    &PATTERNS
}

/// `isContextOverflow` — three branches, see upstream docs.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    // Upstream guards with `if (contextWindow && ...)`: 0 is falsy in JS, so
    // a zero/unknown window disables Cases 2-3 entirely.
    let context_window = context_window.filter(|window| *window > 0);
    // Case 1: error message patterns.
    if message.stop_reason == StopReason::Error
        && let Some(error_message) = &message.error_message
    {
        let is_non_overflow = non_overflow_patterns()
            .iter()
            .any(|pattern| pattern.is_match(error_message));
        if !is_non_overflow {
            if overflow_patterns()
                .iter()
                .any(|pattern| pattern.is_match(error_message))
            {
                return true;
            }
            // #9482: bodyless 400/413 is overflow only from Cerebras.
            if message.provider == "cerebras"
                && cerebras_bodyless_overflow_pattern().is_match(error_message)
            {
                return true;
            }
        }
    }

    // Case 2: silent overflow (z.ai style) — successful but usage exceeds context.
    if let Some(window) = context_window
        && message.stop_reason == StopReason::Stop
    {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if input_tokens > window {
            return true;
        }
    }

    // Case 3: length-stop overflow (Xiaomi MiMo style) — server truncates
    // oversized input, leaving no room for output.
    if let Some(window) = context_window
        && message.stop_reason == StopReason::Length
        && message.usage.output == 0
    {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if input_tokens as f64 >= window as f64 * 0.99 {
            return true;
        }
    }

    false
}

/// `isRecoverableLength` (32850ef7c): whether a length stop ended below the
/// caller or model's intended output limit. Such responses may be caused by
/// context pressure or provider-side truncation, so callers can make one
/// bounded compact-and-retry attempt. `desired_max_output` must be the
/// original limit before any context-based clamping. (The recovery chain
/// itself is T23 scope.)
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiKind, AssistantRole, Usage};

    fn assistant(stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
        assistant_for_provider(stop_reason, error_message, "openai")
    }

    /// #9482 (661619e87): bodyless-overflow cases need a provider-aware
    /// fixture (upstream `createErrorMessage(message, provider)`).
    fn assistant_for_provider(
        stop_reason: StopReason,
        error_message: Option<&str>,
        provider: &str,
    ) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![],
            api: ApiKind::from("openai-completions"),
            provider: provider.to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            error_message: error_message.map(str::to_owned),
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        }
    }

    #[test]
    fn test_overflow_pattern_hits() {
        let cases = [
            "prompt is too long: 213462 tokens > 200000 maximum",
            // #9805 (0e283203c): z.ai's explicit form (`{"code":"1261",
            // "message":"Prompt too long"}`) — upstream overflow.test.ts
            // "detects z.ai prompt-too-long errors".
            "400 {\"code\":\"1261\",\"message\":\"Prompt too long\"}",
            // #10208 (`3dd803d7e`): the z.ai CN endpoint form.
            "400 {\"code\":\"1261\",\"message\":\"Prompt exceeds max length\"}",
            "413 {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}",
            "Your input exceeds the context window of this model",
            "Requested token count exceeds the model's maximum context length of 131072 tokens",
            "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)",
            "This model's maximum prompt length is 131072 but the request contains 537812 tokens",
            "Please reduce the length of the messages or completion",
            "This endpoint's maximum context length is 100 tokens. However, you requested about 200 tokens",
            "The input (100 tokens) is longer than the model's context length (50 tokens).",
            "the request exceeds the available context size, try increasing it",
            "prompt token count of 5 exceeds the limit of 4",
            "invalid params, context window exceeds limit",
            "Your request exceeded model token limit: 5 (requested: 6)",
            "Prompt has 5 tokens, but the configured context size is 4 tokens",
            "Range of input length should be [1, 100]",
            "tokens to keep from the initial prompt is greater than the context length",
            "Prompt contains 100 tokens ... too large for model with 50 maximum context length",
            "prompt too long; exceeded max context length by 3 tokens",
        ];
        for case in cases {
            assert!(
                is_context_overflow(&assistant(StopReason::Error, Some(case)), None),
                "should detect: {case}"
            );
        }
    }

    #[test]
    fn test_non_overflow_exclusions() {
        let cases = [
            "Throttling error: Too many tokens, please wait before trying again.",
            "Service unavailable: Too many tokens",
            "rate limit exceeded: too many tokens",
            "too many requests",
        ];
        for case in cases {
            assert!(
                !is_context_overflow(&assistant(StopReason::Error, Some(case)), None),
                "should exclude: {case}"
            );
        }
    }

    /// #9482 (661619e87 @ d1230ea20): bodyless 400/413 counts as overflow
    /// only for Cerebras; other providers keep it unclassified.
    #[test]
    fn test_bodyless_overflow_scoped_to_cerebras() {
        for message in ["400 status code (no body)", "413 status code (no body)"] {
            assert!(
                is_context_overflow(
                    &assistant_for_provider(StopReason::Error, Some(message), "cerebras"),
                    Some(131_072)
                ),
                "cerebras should detect: {message}"
            );
            assert!(
                !is_context_overflow(
                    &assistant_for_provider(StopReason::Error, Some(message), "opencode-go"),
                    Some(1_000_000)
                ),
                "non-cerebras should not detect: {message}"
            );
        }
    }

    #[test]
    fn test_silent_overflow_zai() {
        let mut msg = assistant(StopReason::Stop, None);
        msg.usage.input = 90;
        msg.usage.cache_read = 20;
        assert!(is_context_overflow(&msg, Some(100)));
        assert!(!is_context_overflow(&msg, Some(200)));
        assert!(!is_context_overflow(&msg, None));
        // JS `if (contextWindow && ...)`: 0 is falsy — a zero/unknown window
        // disables the usage-based cases entirely.
        assert!(!is_context_overflow(&msg, Some(0)));
    }

    #[test]
    fn test_truncation_overflow_xiaomi() {
        let mut msg = assistant(StopReason::Length, None);
        msg.usage.input = 99;
        msg.usage.output = 0;
        assert!(is_context_overflow(&msg, Some(100)));
        msg.usage.output = 1;
        assert!(!is_context_overflow(&msg, Some(100)));
    }

    #[test]
    fn test_unrelated_error_not_overflow() {
        assert!(!is_context_overflow(
            &assistant(StopReason::Error, Some("authentication failed")),
            Some(100)
        ));
    }

    // -- isRecoverableLength -------------------------------------------------
    // Ported from overflow.test.ts @ 32850ef7c (4181f66).

    /// `createLengthStopMessage`.
    fn length_stop(input: u64, cache_read: u64, cache_write: u64, output: u64) -> AssistantMessage {
        let mut msg = assistant(StopReason::Length, None);
        msg.usage.input = input;
        msg.usage.cache_read = cache_read;
        msg.usage.cache_write = cache_write;
        msg.usage.output = output;
        msg.usage.total_tokens = input + cache_read + cache_write + output;
        msg
    }

    #[test]
    fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
        let message = length_stop(3, 253584, 25554, 16);
        assert!(is_recoverable_length(&message, 128000));
    }

    #[test]
    fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
        let message = length_stop(4062, 0, 0, 1024);
        assert!(!is_recoverable_length(&message, 1024));
    }

    #[test]
    fn treats_zero_output_length_stops_as_recoverable_without_context_metadata() {
        let message = length_stop(100, 0, 0, 0);
        assert!(is_recoverable_length(&message, 128000));
    }

    #[test]
    fn zero_desired_max_output_is_never_recoverable() {
        // JS `desiredMaxOutput > 0` guard: an unknown limit disables recovery.
        let message = length_stop(100, 0, 0, 0);
        assert!(!is_recoverable_length(&message, 0));
    }
}
