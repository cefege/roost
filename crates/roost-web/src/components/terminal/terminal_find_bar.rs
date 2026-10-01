//! Find-in-scrollback bar for one terminal pane. Rendered by `CellTerminal`
//! ABOVE the display and INSIDE the pane, so it genuinely consumes rows: the
//! pane's ResizeObserver re-claims the smaller viewport and the shell reflows to
//! match. That is deliberate — a painting pane must have TRUTHFUL geometry, so
//! faking or compensating the height is not an option.
//!
//! Presentation and the in-bar key handling only. The query, the match list, the
//! cursor and the reveal are the `TerminalFind` controller's, and this file
//! renders what its publication says. Ports
//! `apps/web/src/components/terminal/TerminalFindBar.tsx`.
//!
//! The bar owns the keyboard while it is open: `Enter` and `Mod+G` step, `Esc`
//! dismisses, and every other key belongs to the input. It is rendered as a
//! `role="search"` landmark so the count is announced as the reader types.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize};

/// What the bar shows: one query's published state, as the controller reports it.
///
/// A value rather than several props, because the bar re-renders on the whole
/// publication and reading three signals independently would let it paint a
/// count from one search beside a query from another.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindBarState {
    /// The bar is showing.
    pub open: bool,
    /// The current query.
    pub query: String,
    /// The 1-based position of the active match, 0 when there is none.
    pub index: u32,
    /// How many matches were found.
    pub total: u32,
    /// Older matches are known to exist beyond the published ones.
    pub truncated: bool,
    /// The search stopped on something invalid or incomplete. Shown on the
    /// input, never as a toast: a bar showing no results reads as "no matches",
    /// one showing a failure reads as "this did not work".
    pub failed: bool,
    /// The scan is case-sensitive.
    pub case_sensitive: bool,
    /// The query is a pattern rather than a literal.
    pub regex: bool,
    /// Alt-screen has no scrollback, so the search can only cover what is on
    /// screen. Said plainly rather than implying depth.
    pub alt_screen: bool,
}

/// The count the reader reads, as v2 renders it.
///
/// Empty before a query is typed, `0/0` for a query that matched nothing, and
/// `n/total` with a trailing `+` when older matches are known to exist. The
/// truncated marker is the whole point: without it a capped page reads as the
/// complete answer.
#[must_use]
pub fn find_count_text(state: &FindBarState) -> String {
    if state.total == 0 {
        return if state.query.is_empty() {
            String::new()
        } else {
            "0/0".to_owned()
        };
    }
    let truncated = if state.truncated { "+" } else { "" };
    format!("{}/{}{truncated}", state.index, state.total)
}

/// The find bar for one pane.
#[component]
pub fn TerminalFindBar(
    state: FindBarState,
    on_query: EventHandler<String>,
    on_step: EventHandler<i64>,
    on_toggle_case: EventHandler<()>,
    on_toggle_regex: EventHandler<()>,
    on_dismiss: EventHandler<()>,
) -> Element {
    // `FOCUS_OWNERS` in the pane's input lists `input`, so the pane's
    // mousedown-PREVENT / keydown-RECOVER guards leave this alone instead of
    // yanking focus to the hidden textarea mid-typing. The value is bound, so
    // the pane's own query is what the field shows.
    let on_mounted = move |event: MountedEvent| {
        use dioxus::web::WebEventExt as _;
        use wasm_bindgen::JsCast as _;
        if let Some(input) = event.try_as_web_event()
            && let Ok(input) = input.dyn_into::<web_sys::HtmlInputElement>()
        {
            let _ = input.focus();
        }
    };
    let on_key_down = move |event: KeyboardEvent| {
        use dioxus::web::WebEventExt as _;
        // The native event is read rather than `KeyboardData`: a find bar that
        // claimed a keystroke the browser had already defaulted would swallow
        // the field's own text editing along with it.
        let Some(native) = event.try_as_web_event() else {
            return;
        };
        let key = native.key();
        let Some(decision) = find_bar_key(
            &key,
            native.meta_key() || native.ctrl_key(),
            native.shift_key(),
        ) else {
            return;
        };
        event.prevent_default();
        event.stop_propagation();
        match decision {
            FindBarKey::Dismiss => on_dismiss.call(()),
            FindBarKey::Step(delta) => on_step.call(delta),
        }
    };
    let count = find_count_text(&state);
    let no_matches = state.total == 0;
    let alt_label = if state.alt_screen {
        "Find in visible rows"
    } else {
        "Find in scrollback"
    };
    let placeholder = if state.alt_screen {
        "Find (visible rows only)"
    } else {
        "Find in scrollback"
    };
    rsx! {
        div {
            class: "term-find-bar",
            "data-testid": "terminal-find-bar",
            role: "search",
            onkeydown: on_key_down,
            input {
                class: "term-find-input",
                "data-testid": "terminal-find-input",
                "data-failed": if state.failed { "true" } else { "false" },
                r#type: "text",
                spellcheck: "false",
                autocomplete: "off",
                autocapitalize: "off",
                "aria-label": alt_label,
                placeholder,
                value: state.query.clone(),
                onmounted: on_mounted,
                oninput: move |event: FormEvent| on_query.call(event.value()),
            }
            span {
                class: "term-find-count",
                "data-testid": "terminal-find-count",
                {count}
            }
            IconButton {
                icon: "keyboard_arrow_up",
                label: "Previous match",
                size: IconButtonSize::IconSm,
                disabled: no_matches,
                "data-testid": "terminal-find-prev",
                onclick: move |_| on_step.call(-1),
            }
            IconButton {
                icon: "keyboard_arrow_down",
                label: "Next match",
                size: IconButtonSize::IconSm,
                disabled: no_matches,
                "data-testid": "terminal-find-next",
                onclick: move |_| on_step.call(1),
            }
            Button {
                variant: if state.case_sensitive { ButtonVariant::Secondary } else { ButtonVariant::Outline },
                size: ButtonSize::Xs,
                class: "term-find-toggle",
                "data-testid": "terminal-find-case",
                "data-on": if state.case_sensitive { "true" } else { "false" },
                "data-active": if state.case_sensitive { "true" } else { "false" },
                "aria-pressed": if state.case_sensitive { "true" } else { "false" },
                title: "Match case",
                onclick: move |_| on_toggle_case.call(()),
                "Aa"
            }
            Button {
                variant: if state.regex { ButtonVariant::Secondary } else { ButtonVariant::Outline },
                size: ButtonSize::Xs,
                class: "term-find-toggle",
                "data-testid": "terminal-find-regex",
                "data-on": if state.regex { "true" } else { "false" },
                "data-active": if state.regex { "true" } else { "false" },
                "aria-pressed": if state.regex { "true" } else { "false" },
                title: "Regular expression",
                onclick: move |_| on_toggle_regex.call(()),
                ".*"
            }
            if state.alt_screen {
                span { class: "term-find-note", "visible rows only" }
            }
            IconButton {
                icon: "close",
                label: "Close find",
                size: IconButtonSize::IconSm,
                "data-testid": "terminal-find-close",
                onclick: move |_| on_dismiss.call(()),
            }
        }
    }
}

/// What one keypress in the bar means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindBarKey {
    /// Close the bar and hand the keyboard back to the PTY.
    Dismiss,
    /// Move the active match, wrapping at both ends.
    Step(i64),
}

/// The keys the bar claims while it is open.
///
/// `Esc` dismisses; `Enter` steps forward and `Shift+Enter` steps back; `Mod+G`
/// is the same step so the reader never has to reach for the arrow. Everything
/// else belongs to the input, including every printable character — a find bar
/// that ate a letter would be searching for a query the reader never typed.
#[must_use]
pub fn find_bar_key(key: &str, modifier: bool, shift: bool) -> Option<FindBarKey> {
    match key {
        "Escape" => Some(FindBarKey::Dismiss),
        "Enter" => Some(FindBarKey::Step(if shift { -1 } else { 1 })),
        _ if modifier && key.eq_ignore_ascii_case("g") => {
            Some(FindBarKey::Step(if shift { -1 } else { 1 }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(total: u32, index: u32, query: &str, truncated: bool) -> FindBarState {
        FindBarState {
            open: true,
            query: query.to_owned(),
            index,
            total,
            truncated,
            ..FindBarState::default()
        }
    }

    #[test]
    fn the_count_is_empty_until_the_reader_asks_for_something() {
        // An empty bar reading "0/0" tells the reader their search found nothing
        // before they have run one.
        assert_eq!(find_count_text(&FindBarState::default()), "");
    }

    #[test]
    fn a_query_that_matched_nothing_says_so_rather_than_going_blank() {
        // Blank is the no-query reading; "0/0" is the searched-and-empty one.
        // Collapsing them would report a failed scan as an empty history.
        let searched = state_with(0, 0, "FINDLINE-400", false);
        assert_eq!(find_count_text(&searched), "0/0");
    }

    #[test]
    fn a_capped_page_says_it_is_capped() {
        // Without the `+`, a page that stopped at the match ceiling reads as the
        // whole answer, and the reader concludes the needle is unique.
        let capped = state_with(256, 1, "needle", true);
        assert_eq!(find_count_text(&capped), "1/256+");
        let whole = state_with(2, 1, "needle", false);
        assert_eq!(find_count_text(&whole), "1/2");
    }

    #[test]
    fn every_letter_typed_into_the_bar_belongs_to_the_query() {
        // The pane's hidden textarea is the PTY's; a key the bar consumed would
        // search for a query the reader never typed AND steal the character from
        // the field that is showing it.
        for key in ["a", "Z", "1", " ", "/", "ArrowDown", "Tab", "Backspace"] {
            assert_eq!(find_bar_key(key, false, false), None, "{key} was claimed");
        }
    }

    #[test]
    fn escape_dismisses_and_enter_steps() {
        assert_eq!(
            find_bar_key("Escape", false, false),
            Some(FindBarKey::Dismiss)
        );
        assert_eq!(
            find_bar_key("Enter", false, false),
            Some(FindBarKey::Step(1))
        );
        assert_eq!(
            find_bar_key("Enter", false, true),
            Some(FindBarKey::Step(-1))
        );
        // `Mod+G` is the keyboard-only way to reach the next match.
        assert_eq!(find_bar_key("g", true, false), Some(FindBarKey::Step(1)));
        assert_eq!(find_bar_key("G", true, true), Some(FindBarKey::Step(-1)));
        // Without the modifier it is a letter, and the reader is typing.
        assert_eq!(find_bar_key("g", false, false), None);
    }
}
