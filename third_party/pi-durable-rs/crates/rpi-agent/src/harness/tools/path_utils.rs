//! Port of `packages/agent/src/harness/tools/path-utils.ts` @ pi 0.82.1
//! (2efa728) — path normalization and resolution for the built-in tools.
//!
//! Intentional differences:
//! - `AbortSignal | undefined` is `Option<CancellationToken>` (harness-wide
//!   convention, see `harness/types.rs` header).
//! - `getOrThrow` (path-utils.ts:2) is `?` with `AgentError::Message`, keeping
//!   the upstream error text verbatim.
//! - The `(AM|PM)` regex replacement is a manual char-window scan; the regex
//!   matches ASCII only, so scanning `Vec<char>` is exact.

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use crate::error::AgentError;
use crate::harness::types::ExecutionEnv;

/// `UNICODE_SPACES` (path-utils.ts:4) — the space variants replaced with a
/// regular space: U+00A0 NBSP, U+2000-U+200A various spaces, U+202F narrow
/// NBSP, U+205F medium math space, U+3000 ideographic space.
fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// `NARROW_NO_BREAK_SPACE` (path-utils.ts:5) — used by macOS in screenshot
/// filenames before AM/PM.
pub const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// `normalizeToolPath` (path-utils.ts:7-10): replace Unicode space variants
/// with a regular space and strip a leading `@`.
fn normalize_tool_path(path: &str) -> String {
    let normalized: String = path
        .chars()
        .map(|c| if is_unicode_space(c) { ' ' } else { c })
        .collect();
    match normalized.strip_prefix('@') {
        Some(rest) => rest.to_string(),
        None => normalized,
    }
}

/// `resolveToolPath` (path-utils.ts:12-14).
pub async fn resolve_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    signal: Option<CancellationToken>,
) -> Result<String, AgentError> {
    env.absolute_path(&normalize_tool_path(path), signal)
        .await
        .map_err(|error| AgentError::Message(error.message))
}

/// `tryMacOSScreenshotPath` (path-utils.ts:17-20 of the coding-agent variant;
/// the harness inlines the replacement at path-utils.ts:20): replace
/// `" AM."` / `" PM."` (case-insensitive) with `<NNBSP>AM.` / `<NNBSP>PM.`.
/// The matched letters keep their original case (JS `$1` group replacement).
fn try_macos_screenshot_path(file_path: &str) -> String {
    let chars: Vec<char> = file_path.chars().collect();
    let mut result = String::with_capacity(file_path.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' '
            && i + 3 < chars.len()
            && chars[i + 3] == '.'
            && matches!(
                (chars[i + 1], chars[i + 2]),
                ('A' | 'a', 'M' | 'm') | ('P' | 'p', 'M' | 'm')
            )
        {
            result.push(NARROW_NO_BREAK_SPACE);
            result.push(chars[i + 1]);
            result.push(chars[i + 2]);
            result.push('.');
            i += 4;
        } else {
            result.push(c);
            i += 1;
        }
    }
    result
}

/// `resolveReadToolPath` (path-utils.ts:16-29): resolve the path, then try
/// macOS filename variants — narrow no-break space before AM/PM, NFD
/// decomposition, U+2019 curly apostrophe for `'`, and NFD + curly apostrophe.
/// The first existing variant wins; otherwise the plain resolution is returned.
pub async fn resolve_read_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    signal: Option<CancellationToken>,
) -> Result<String, AgentError> {
    let resolved = resolve_tool_path(env, path, signal.clone()).await?;
    let nfd: String = resolved.chars().nfd().collect();
    let variants = [
        resolved.clone(),
        try_macos_screenshot_path(&resolved),
        nfd.clone(),
        resolved.replace('\'', "\u{2019}"),
        nfd.replace('\'', "\u{2019}"),
    ];
    // `new Set(variants)` — dedupe before probing (path-utils.ts:26).
    let mut seen = HashSet::new();
    for variant in variants {
        if !seen.insert(variant.clone()) {
            continue;
        }
        if env
            .exists(&variant, signal.clone())
            .await
            .map_err(|error| AgentError::Message(error.message))?
        {
            return Ok(variant);
        }
    }
    Ok(resolved)
}
