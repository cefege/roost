//! The terminal scroller's container CSS. Exercises
//! `crates/roost-web/assets/styles/terminal-grid.css`.
//!
//! The core populates scrollback and the renderer emits its rows; only this
//! stylesheet lets them be scrolled, so a lost rule reads as "the wheel does
//! nothing in the terminal".

const TERMINAL_GRID_CSS: &str = include_str!("../assets/styles/terminal-grid.css");

/// `css` with every `/* … */` comment removed, so prose with `;` or `:` in it
/// is never read as a declaration.
fn without_comments(css: &str) -> String {
    let mut uncommented = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        uncommented.push_str(&rest[..start]);
        rest = rest[start..]
            .split_once("*/")
            .map_or("", |(_, after)| after);
    }
    uncommented.push_str(rest);
    uncommented
}

/// The declarations of the first rule whose selector is exactly `selector`.
fn rule_body(css: &str, selector: &str) -> Option<String> {
    let opener = format!("\n{selector} {{");
    let start = css.find(&opener)? + opener.len();
    css[start..]
        .split_once('}')
        .map(|(body, _)| body.to_owned())
}

#[test]
fn the_terminal_scroller_keeps_its_scrollbar() {
    let body = rule_body(&without_comments(TERMINAL_GRID_CSS), ".wterm")
        .expect("terminal-grid.css declares a `.wterm` rule");
    let declarations = body
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .map(|(property, value)| (property.trim(), value.trim()))
        .collect::<Vec<_>>();
    for expected in [
        ("overflow-y", "auto"),
        ("overflow-x", "hidden"),
        ("scrollbar-gutter", "stable"),
    ] {
        assert!(
            declarations.contains(&expected),
            "`.wterm` lost `{}: {}`; scrollback rows can no longer be scrolled",
            expected.0,
            expected.1
        );
    }
}
