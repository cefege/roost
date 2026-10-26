//! Port of `packages/ai/src/api/openai-prompt-cache.ts` @ pi 0.82.1 (2efa728).

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// `clampOpenAIPromptCacheKey`: truncate to 64 chars (`Array.from` upstream
/// counts UTF-16 code units; this counts `char`s — BMP-equivalent, see D-003).
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_owned());
    }
    Some(
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_openai_prompt_cache_key() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some("short")),
            Some("short".to_owned())
        );
        let long = "x".repeat(100);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some(&long)),
            Some("x".repeat(64))
        );
    }
}
