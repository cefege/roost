//! Port of `packages/ai/src/utils/sanitize-unicode.ts` @ pi 0.82.1 (2efa728).
//!
//! Intentional difference: Rust `String`/`&str` is always valid UTF-8 and can
//! never contain unpaired surrogates, so this is the identity function. Lone
//! surrogates entering through `\uXXXX` JSON escapes are handled at the JSON
//! parse boundary instead (see `utils/json_parse.rs`:
//! `strip_lone_surrogate_escapes`), which reproduces the net effect of the
//! upstream pipeline (`JSON.parse` keeps lone surrogates; `sanitizeSurrogates`
//! drops them at the call sites that use this function).
//!
//! The function is kept so call sites mirror upstream 1:1.

/// `sanitizeSurrogates` — identity over valid UTF-8 (see module docs).
pub fn sanitize_surrogates(text: &str) -> &str {
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_surrogates_preserves_valid_text() {
        assert_eq!(sanitize_surrogates("Hello 🙈 World"), "Hello 🙈 World");
        assert_eq!(sanitize_surrogates(""), "");
    }
}
