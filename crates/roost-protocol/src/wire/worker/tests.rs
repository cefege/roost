//! Host identity normalization as the OS actually reports it.
//!
//! Every case feeds the normalizer text a machine can really produce — control
//! characters, bidi overrides, run-collapsing whitespace, and fields far past
//! the bound — because the security claim is about what a hostile field reduces
//! to, not about a well-behaved one.

use super::*;

#[test]
fn a_host_field_is_reduced_to_one_line_without_controls() {
    let noisy = serde_json::json!("Apple\u{0}\u{1b} M2\u{200e}\n  Pro\u{a0}Book\u{7}");
    assert_eq!(
        normalize_host_identity_text(&noisy).as_deref(),
        Some("Apple M2 Pro Book")
    );
}

#[test]
fn whitespace_becomes_one_space_and_a_format_character_is_dropped() {
    // The two rules are ordered, and the order is the behaviour: whitespace
    // is tested FIRST, so a control character that is also whitespace — the
    // vertical tab is both — becomes a space rather than vanishing. Only a
    // control or format character that is not whitespace is dropped, which
    // is what the zero-width joiner is.
    let vertical_tab = serde_json::json!("a\u{b}c\u{200d}d");
    assert_eq!(
        normalize_host_identity_text(&vertical_tab).as_deref(),
        Some("a cd")
    );

    // A format character on its own leaves nothing, so the field is empty
    // and the whole identity is refused rather than half-populated.
    let only_format = serde_json::json!("\u{200d}");
    assert_eq!(normalize_host_identity_text(&only_format), None);

    // A run of whitespace collapses to one space and the ends are trimmed.
    let runs = serde_json::json!("  x \t\n y  ");
    assert_eq!(normalize_host_identity_text(&runs).as_deref(), Some("x y"));
}

#[test]
fn a_non_string_and_an_empty_string_are_both_nothing() {
    for value in [
        serde_json::json!(7),
        serde_json::json!(""),
        serde_json::json!("   "),
    ] {
        assert_eq!(normalize_host_identity_text(&value), None);
    }
    assert_eq!(normalize_host_identity_text(&Value::Null), None);
}

#[test]
fn a_host_field_is_cut_at_the_byte_bound_not_the_character_bound() {
    // 129 three-byte characters are 387 bytes, so the 256-byte bound lands
    // mid-run and must not split one.
    let value = serde_json::json!("一".repeat(129));
    let normalized = normalize_host_identity_text(&value).expect("not empty");
    assert!(normalized.len() <= HOST_IDENTITY_VALUE_MAX_UTF8_BYTES);
    assert_eq!(normalized.chars().count(), 85);
}

#[test]
fn a_host_field_longer_than_the_bound_stops_at_the_bound() {
    let value = serde_json::json!("a".repeat(HOST_IDENTITY_MAX_INSPECTED_CODE_UNITS * 2));
    let normalized = normalize_host_identity_text(&value).expect("not empty");
    assert_eq!(normalized, "a".repeat(HOST_IDENTITY_VALUE_MAX_UTF8_BYTES));
}

#[test]
fn an_identity_of_three_nulls_describes_no_machine() {
    assert_eq!(normalize_host_identity(&Value::Null), None);
    assert_eq!(normalize_host_identity(&serde_json::json!([])), None);
    assert_eq!(normalize_host_identity(&serde_json::json!("Apple")), None);
    assert_eq!(
        normalize_host_identity(&serde_json::json!({
            "hardware_model": null,
            "chip": "M2",
        })),
        Some(HostIdentity {
            hardware_model: None,
            chip: Some("M2".to_owned()),
            linux_distribution: None,
        })
    );
}
