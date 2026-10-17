//! The status dot's token map and its inline style: filled and hollow dots set
//! every colour longhand, and no token rides a shorthand that a re-render would
//! erase. Exercises `roost_web::components::ui::status`.

use roost_web::components::ui::status::{status_dot_style, status_dot_token};

#[test]
fn every_status_maps_to_its_canonical_token() {
    for (status, token) in [
        ("ok", "--status-ok"),
        ("done", "--status-ok"),
        ("running", "--md-primary"),
        ("idle", "--text-lo"),
        ("offline", "--text-lo"),
        ("warn", "--status-warn"),
        ("error", "--status-err"),
        ("info", "--status-info"),
        ("something-new", "--text-lo"),
    ] {
        assert_eq!(status_dot_token(status), token, "{status}");
    }
}

#[test]
fn a_status_dot_is_filled_or_ringed_at_its_size() {
    let solid = status_dot_style("error", 8, false);
    assert!(solid.contains("width: 8px; height: 8px;"), "{solid}");
    assert!(
        solid.contains("background-color: var(--status-err);"),
        "{solid}"
    );
    assert!(solid.contains("border-color: transparent;"), "{solid}");
    let hollow = status_dot_style("ok", 12, true);
    assert!(hollow.contains("width: 12px; height: 12px;"), "{hollow}");
    assert!(
        hollow.contains("background-color: transparent;"),
        "{hollow}"
    );
    assert!(
        hollow.contains("border-color: var(--status-ok);"),
        "{hollow}"
    );
}

/// Dioxus re-applies, after every `style` update, each property the new value
/// reports as empty, and a shorthand holding `var()` reports its longhands
/// empty: the update erases the colour. A dot's token must ride a longhand.
#[test]
fn a_status_dot_never_puts_its_token_in_a_shorthand() {
    for (status, hollow) in [
        ("ok", false),
        ("running", false),
        ("idle", true),
        ("error", true),
    ] {
        let style = status_dot_style(status, 8, hollow);
        for declaration in style
            .split(';')
            .map(str::trim)
            .filter(|part| part.contains("var("))
        {
            let property = declaration.split(':').next().unwrap_or_default().trim();
            assert!(
                property.contains('-'),
                "{property} is a shorthand carrying a token in {style}"
            );
        }
    }
}
