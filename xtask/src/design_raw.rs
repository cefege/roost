//! The design-system raw-value ratchet over crates/roost-web. A raw hex color,
//! an `rgb()`/`rgba()` literal, or a px font-size outside a token-definition
//! file is how a new surface drifts away from the palette. The token files
//! DECLARE those values, so they are exempt; everything else must reference
//! them through `var(--…)`. Ported from scripts/lint-roost.ts, re-rooted on
//! the Rust web crate.

use crate::ratchet::{RatchetOutcome, RatchetSpec, count_matching_lines, run_ratchet};
use crate::source_tree;

const BASELINE: &str = "xtask/design-raw-baseline.json";
const RULE: &str = "design: no NEW raw color/px-font values — use --md-*/--surface-*/--text-* + the type ramp (ratcheted)";
const MEMORY: &str = "CLAUDE.md — design system";
/// The files that declare the tokens. Everything else references them.
const TOKEN_FILES: [&str; 3] = [
    "assets/theme-vars.css",
    "assets/md-tokens.css",
    "assets/syntax-vars.css",
];

fn describe(observed: usize, allowed: usize) -> String {
    format!(
        "{observed} raw hex/rgb/px-font value lines (baseline {allowed}) — reference a theme token instead"
    )
}

fn spec() -> RatchetSpec {
    RatchetSpec {
        fresh_allowance: 0,
        guard_floor: 0,
        rule: RULE,
        memory: MEMORY,
        describe,
    }
}

fn is_scanned(relative: &str) -> bool {
    let is_source = relative.ends_with(".rs") || relative.ends_with(".css");
    is_source
        && !relative.contains("/target/")
        && !TOKEN_FILES.iter().any(|token| relative.ends_with(token))
}

fn is_raw_value_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return false;
    }
    // Strip var(--token, …) fallbacks so their inner literal is not counted
    // twice; a hardcoded color fallback is a separate, unconditional rule.
    let without_fallbacks = strip_var_fallbacks(trimmed);
    has_hex_color(&without_fallbacks)
        || without_fallbacks.contains("rgb(")
        || without_fallbacks.contains("rgba(")
        || has_px_font_size(trimmed)
}

fn strip_var_fallbacks(line: &str) -> String {
    let mut stripped = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("var(") {
        let tail = &rest[at..];
        match tail.find(')') {
            Some(close) => {
                stripped.push_str(&rest[..at]);
                rest = &tail[close + 1..];
            }
            None => break,
        }
    }
    stripped.push_str(rest);
    stripped
}

fn has_hex_color(line: &str) -> bool {
    let bytes: Vec<char> = line.chars().collect();
    bytes.iter().enumerate().any(|(index, character)| {
        *character == '#'
            && bytes
                .get(index + 1..index + 4)
                .is_some_and(|digits| digits.iter().all(|d| d.is_ascii_hexdigit()))
    })
}

fn has_px_font_size(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase().replace(['-', ' '], "");
    let Some(at) = lowered.find("fontsize:") else {
        return false;
    };
    let value = lowered[at + "fontsize:".len()..].trim_start_matches(['\'', '"']);
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    !digits.is_empty() && value[digits.len()..].starts_with("px")
}

pub fn run(update_baseline: bool) -> RatchetOutcome {
    let web = source_tree::repo_root().join("crates/roost-web");
    let counts = count_matching_lines(&web, is_scanned, is_raw_value_line);
    if update_baseline && let Err(error) = crate::ratchet::write_baseline(BASELINE, &counts) {
        eprintln!("xtask: cannot write {BASELINE}: {error}");
        std::process::exit(1);
    }
    run_ratchet(
        &counts,
        &crate::ratchet::read_baseline(BASELINE),
        &spec(),
        update_baseline,
    )
}
#[cfg(test)]
mod tests {
    use super::{has_hex_color, has_px_font_size, is_raw_value_line, strip_var_fallbacks};

    #[test]
    fn flags_a_raw_hex_color() {
        assert!(is_raw_value_line("    color: #ff8800;"));
        assert!(is_raw_value_line(
            "    border: 1px solid rgba(1, 2, 3, 0.5);"
        ));
    }

    #[test]
    fn flags_a_px_font_size() {
        assert!(is_raw_value_line("    font-size: 14px;"));
        assert!(is_raw_value_line("    font-size:24px;"));
    }

    #[test]
    fn ignores_a_color_declared_inside_a_token_fallback() {
        // The fallback check owns `var(--token, #hex)`; counting it here too
        // would report one line twice and make the ratchet unusable.
        assert!(!is_raw_value_line("    color: var(--text-hi, #ff8800);"));
    }

    #[test]
    fn a_hash_without_three_hex_digits_is_not_a_color() {
        assert!(!has_hex_color("    content: \"#\";"));
        assert!(!has_hex_color("    grid-area: 1 / 2;"));
        assert!(has_hex_color("    color: #abc;"));
    }

    #[test]
    fn only_font_size_px_counts_not_any_px_length() {
        assert!(has_px_font_size("    font-size: 14px;"));
        assert!(!has_px_font_size("    padding: 14px;"));
        assert!(!has_px_font_size("    font-size: var(--md-size-3);"));
    }

    #[test]
    fn ignores_comment_lines() {
        assert!(!is_raw_value_line("// color: #ff8800;"));
        assert!(!is_raw_value_line("   /* font-size: 14px; */"));
    }

    #[test]
    fn keeps_text_around_a_stripped_fallback() {
        assert_eq!(
            strip_var_fallbacks(
                "color: var(--text-hi, #ff8800); background: var(--surface-0, #101010);"
            ),
            "color: ; background: ;"
        );
    }
}
