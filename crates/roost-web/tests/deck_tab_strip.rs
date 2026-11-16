//! A pane tab's outer width stays definite and bounded by the tab-width
//! tokens. Exercises `components/deck/pane_tab.rs` and
//! `assets/styles/theme-vars.css`.
//!
//! An inline-size container zeroes its intrinsic contribution, so a tab with
//! only a flex basis collapses every tab in the strip to its floor width.

const PANE_TAB_SOURCE: &str = include_str!("../src/components/deck/pane_tab.rs");
const THEME_VARS_CSS: &str = include_str!("../assets/styles/theme-vars.css");

/// The class tokens of the tab's root element.
fn tab_root_classes() -> Vec<&'static str> {
    PANE_TAB_SOURCE
        .lines()
        .find(|line| line.contains("class: \"group/tab "))
        .expect("pane_tab.rs renders a `group/tab` root")
        .split('"')
        .nth(1)
        .expect("the class literal is quoted")
        .split_whitespace()
        .collect()
}

#[test]
fn the_pane_tab_width_is_bounded_by_the_tab_width_token() {
    let classes = tab_root_classes();
    for required in [
        "w-[var(--workbench-tab-width-max)]",
        "min-w-[var(--workbench-tab-width-min)]",
        "max-w-[var(--workbench-tab-width-max)]",
    ] {
        assert!(
            classes.contains(&required),
            "the tab root lost `{required}`; its flex base size would collapse to the floor"
        );
    }
    for token in ["--workbench-tab-width-max", "--workbench-tab-width-min"] {
        assert!(
            THEME_VARS_CSS.contains(&format!("{token}:")),
            "theme-vars.css no longer declares `{token}`"
        );
    }
}
