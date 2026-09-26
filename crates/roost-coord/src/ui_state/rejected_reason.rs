//! Bounded, control-free rendering of a browser's own layout rejection reason.
//!
//! The reason a target tab sends back is echoed to the CALLING browser, so it is
//! untrusted text on a path that ends in somebody's UI. v2's
//! `sanitizeRejectedReason` (`ui-layout-apply-owner.ts:321-345`) collapses
//! whitespace, drops control and format characters, and stops at 200 code
//! points; this is that function. Called from `layout_apply` only.

/// The longest reason a caller is told, in code points.
pub const UI_LAYOUT_REJECTED_REASON_MAX_LENGTH: usize = 200;

/// How many code points are examined before the output cap is assumed to have
/// been reached. Four times the output cap: every character that is dropped
/// costs one inspection, so a run of invisible characters cannot make the
/// sanitiser read an unbounded string.
const MAX_INSPECTED_CODE_POINTS: usize = UI_LAYOUT_REJECTED_REASON_MAX_LENGTH * 4;

/// The reason a rejection carries when the browser supplied nothing usable.
const REJECTED_REASON_FALLBACK: &str = "layout apply rejected";

/// The Unicode `Cf` (format) ranges a browser UI can be misled by, in order.
///
/// v2 filters the whole `Cf` category through a regular expression. Rust's
/// standard library has no general-category predicate, so this table names the
/// ranges that carry invisible or direction-changing formatting: zero-width
/// joiners and spaces, bidirectional overrides and isolates, the Arabic and
/// Mongolian formatting controls, the byte-order mark, and the interlinear
/// annotation marks. It is a hardening, not a validator -- a reason that
/// survives it is still arbitrary text from another device.
const FORMAT_RANGES: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x180E, 0x180E),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
];

/// Whether a character is invisible formatting rather than readable text.
fn is_format_character(character: char) -> bool {
    let code = u32::from(character);
    FORMAT_RANGES
        .iter()
        .any(|(first, last)| code >= *first && code <= *last)
}

/// The browser's reason, bounded and stripped, or the fallback.
#[must_use]
pub fn sanitized_rejected_reason(reason: Option<&str>) -> String {
    let mut clean = String::new();
    let mut output_code_points = 0usize;
    let mut pending_space = false;
    for (inspected_code_points, character) in reason.unwrap_or_default().chars().enumerate() {
        if inspected_code_points >= MAX_INSPECTED_CODE_POINTS
            || output_code_points >= UI_LAYOUT_REJECTED_REASON_MAX_LENGTH
        {
            break;
        }
        if character.is_control() || character.is_whitespace() || is_format_character(character) {
            if output_code_points > 0 {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            if output_code_points + 1 >= UI_LAYOUT_REJECTED_REASON_MAX_LENGTH {
                break;
            }
            clean.push(' ');
            output_code_points += 1;
            pending_space = false;
        }
        clean.push(character);
        output_code_points += 1;
    }
    if clean.is_empty() {
        return REJECTED_REASON_FALLBACK.to_owned();
    }
    clean
}
