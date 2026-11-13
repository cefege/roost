//! Styled ANSI lines shared by tool output and native content.
//! Producer text, SGR and OSC8 links survive parsing; windows own output limits.
//! Color and decoration rendering delegates to the terminal grid's style model.

mod cursor;
mod presentation;
mod style;

pub(super) use presentation::render_run;
use std::sync::Arc;
pub(super) use style::RunStyle;

/// One coloured run of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Run {
    pub text: String,
    pub style: RunStyle,
    pub href: Option<Arc<str>>,
}

/// How a diff line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineTone {
    Plain,
    Added,
    Removed,
    Header,
}

impl LineTone {
    /// The `data-tone` value, `None` for plain lines.
    pub fn attribute(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Added => Some("added"),
            Self::Removed => Some("removed"),
            Self::Header => Some("header"),
        }
    }
}

/// One output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutputLine {
    pub runs: Vec<Run>,
    pub tone: LineTone,
}

/// Whether `tool_name`'s output, or the text itself, is a unified diff.
pub(super) fn reads_as_diff(tool_name: &str, text: &str) -> bool {
    tool_name == "edit" || text.starts_with("diff --git") || text.contains("\n@@ ")
}

/// `text` split into styled lines; a trailing newline adds no empty line.
pub(super) fn styled_lines(text: &str, diff: bool) -> Vec<OutputLine> {
    decode_lines(text.strip_suffix('\n').unwrap_or(text).split('\n'), diff)
}

pub(super) fn styled_rows<'a>(rows: impl Iterator<Item = &'a str>) -> Vec<OutputLine> {
    decode_lines(rows, false)
}

fn decode_lines<'a>(rows: impl Iterator<Item = &'a str>, diff: bool) -> Vec<OutputLine> {
    let mut lines = Vec::new();
    let mut state = AnsiState::default();
    for raw in rows {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let runs = parse_runs(raw, &mut state);
        let tone = if diff {
            diff_tone(&runs)
        } else {
            LineTone::Plain
        };
        lines.push(OutputLine { runs, tone });
    }
    lines
}

/// The part of `lines` a folded preview shows: all of them when `expanded`,
/// otherwise `rows` from the head or the tail. Returns it and how many are left out.
pub(super) fn window<T>(
    lines: &[T],
    rows: usize,
    from_head: bool,
    expanded: bool,
) -> (&[T], usize) {
    let total = lines.len();
    if expanded || total <= rows {
        return (lines, 0);
    }
    let shown = if from_head {
        &lines[..rows]
    } else {
        &lines[total - rows..]
    };
    (shown, total - rows)
}

fn diff_tone(runs: &[Run]) -> LineTone {
    let first = runs.first().map_or("", |run| run.text.as_str());
    if first.starts_with("+++") || first.starts_with("---") || first.starts_with("@@") {
        LineTone::Header
    } else if first.starts_with('+') {
        LineTone::Added
    } else if first.starts_with('-') {
        LineTone::Removed
    } else {
        LineTone::Plain
    }
}

#[derive(Default)]
struct AnsiState {
    style: RunStyle,
    href: Option<Arc<str>>,
}

fn parse_runs(line: &str, state: &mut AnsiState) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut text = String::new();
    let mut chars = line.chars().peekable();
    let mut cursor = None;
    let flush =
        |runs: &mut Vec<Run>, text: &mut String, state: &AnsiState, cursor: &mut Option<usize>| {
            if !text.is_empty() {
                let run = Run {
                    text: std::mem::take(text),
                    style: state.style,
                    href: state.href.clone(),
                };
                if let Some(column) = cursor.as_mut() {
                    *column = cursor::overwrite(runs, *column, run);
                } else {
                    runs.push(run);
                }
            }
        };
    while let Some(ch) = chars.next() {
        if matches!(ch, '\r' | '\u{8}') {
            flush(&mut runs, &mut text, state, &mut cursor);
            if cursor.is_none() {
                cursor::prepare(&mut runs);
            }
            cursor = Some(if ch == '\r' {
                0
            } else {
                cursor
                    .unwrap_or_else(|| cursor::column(&runs))
                    .saturating_sub(1)
            });
            continue;
        }
        if ch != '\u{1b}' {
            if ch == '\t' || !ch.is_control() {
                text.push(ch);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut parameters = String::new();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        if next == 'm' {
                            flush(&mut runs, &mut text, state, &mut cursor);
                            state.style.apply_sgr(&parameters);
                        }
                        break;
                    }
                    parameters.push(next);
                }
            }
            Some(']') => {
                let mut command = String::new();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                    command.push(next);
                }
                if let Some(command) = command.strip_prefix("8;")
                    && let Some((_, href)) = command.split_once(';')
                {
                    flush(&mut runs, &mut text, state, &mut cursor);
                    state.href = (!href.is_empty()
                        && roost_protocol::cell::link_uri_within_cap(href))
                    .then(|| Arc::from(href));
                }
            }
            Some(_) | None => {}
        }
    }
    flush(&mut runs, &mut text, state, &mut cursor);
    runs
}

#[cfg(test)]
mod tests {
    use super::{LineTone, RunStyle, reads_as_diff, styled_lines, styled_rows, window};
    use roost_protocol::cell::{CELL_BOLD, DEFAULT_COLOR};

    #[test]
    fn sgr_colours_carry_across_lines_until_reset_and_other_escapes_vanish() {
        let lines = styled_lines(
            "\u{1b}[1;31merror\u{1b}]0;title\u{7}\nstill red\u{1b}[0m plain\n",
            false,
        );
        assert_eq!(lines.len(), 2);
        let red_bold = lines[0].runs[0].style;
        assert_eq!(lines[0].runs[0].text, "error");
        assert_eq!(red_bold.fg, 1);
        assert_eq!(red_bold.flags, CELL_BOLD);
        assert_eq!(lines[1].runs[0].style, red_bold);
        assert_eq!(lines[1].runs[1].text, " plain");
        assert_eq!(lines[1].runs[1].style, RunStyle::default());
        assert!(
            red_bold
                .inline_style()
                .contains("color:var(--term-color-1)")
        );
    }

    #[test]
    fn palette_and_truecolor_survive_with_following_attributes() {
        let lines = styled_lines(
            "\u{1b}[92ma\u{1b}[38;5;4mb\u{1b}[38;5;200mc\u{1b}[38;2;1;2;3;1md",
            false,
        );
        let colors: Vec<_> = lines[0]
            .runs
            .iter()
            .map(|run| (run.style.fg, run.style.fg_rgb))
            .collect();
        assert_eq!(
            colors,
            [
                (10, None),
                (4, None),
                (200, None),
                (DEFAULT_COLOR, Some(0x010203))
            ]
        );
        assert_ne!(lines[0].runs[3].style.flags & CELL_BOLD, 0);
    }

    #[test]
    fn a_diff_tones_its_lines_and_a_preview_keeps_one_edge() {
        let text = "--- a\n+++ a\n@@ -1 +1 @@\n-old\n+new\n same\n";
        assert!(reads_as_diff("bash", text));
        let tones: Vec<_> = styled_lines(text, true)
            .iter()
            .map(|line| line.tone)
            .collect();
        assert_eq!(
            tones,
            [
                LineTone::Header,
                LineTone::Header,
                LineTone::Header,
                LineTone::Removed,
                LineTone::Added,
                LineTone::Plain
            ]
        );
        let rows: Vec<u32> = (1..=25).collect();
        assert_eq!(window(&rows, 10, false, false), (&rows[15..], 15));
        assert_eq!(window(&rows, 3, true, false), (&rows[..3], 22));
        assert_eq!(window(&rows, 3, true, true), (&rows[..], 0));
    }

    #[test]
    fn a_redrawn_progress_line_shows_only_its_last_state() {
        let lines = styled_lines("\u{1b}[32m 10%\r 50%\r100% done\r\nnext\n", false);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0]
                .runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "100% done"
        );
        assert_eq!(lines[0].runs[0].style.fg, 2);
        let tag = "1A2B";
        assert!(!reads_as_diff("write", &format!("[a.rs#{tag}]\n")));
    }

    #[test]
    fn long_lines_keep_their_complete_content_and_closing_reset() {
        let long = "x".repeat(5_000);
        let text = format!("\u{1b}[31m{long}\u{1b}[0m tail\nnext\n");
        let lines = styled_lines(&text, false);
        assert_eq!(lines.len(), 2);
        let first: String = lines[0].runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(first, format!("{long} tail"));
        assert_eq!(lines[0].runs[0].style.fg, 1);
        assert_eq!(lines[1].runs[0].text, "next");
        assert_eq!(lines[1].runs[0].style.fg, DEFAULT_COLOR);
    }

    #[test]
    fn osc8_links_and_sgr_are_independent_and_carry_across_lines() {
        let lines = styled_lines(
            "\u{1b}]8;id=link;https://example.com/source\u{7}\u{1b}[31mfirst\nsecond\u{1b}[0m plain\u{1b}]8;;\u{1b}\\ outside",
            false,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].runs[0].href.as_deref(),
            Some("https://example.com/source")
        );
        assert_eq!(
            lines[1].runs[0].href.as_deref(),
            Some("https://example.com/source")
        );
        assert_eq!(
            lines[1].runs[1].href.as_deref(),
            Some("https://example.com/source")
        );
        assert_eq!(lines[1].runs[1].style.fg, DEFAULT_COLOR);
        assert_eq!(lines[1].runs[2].text, " outside");
        assert_eq!(lines[1].runs[2].href, None);
    }

    #[test]
    fn explicit_rows_preserve_empty_rows_and_truecolor_backgrounds() {
        let lines =
            styled_rows(["", "\u{1b}[38:2::12:34:56;48;2;78;90;123mcolored", ""].into_iter());
        assert_eq!(lines.len(), 3);
        assert!(lines[0].runs.is_empty());
        assert!(lines[2].runs.is_empty());
        let style = lines[1].runs[0].style;
        assert_eq!(style.fg_rgb, Some(0x0c2238));
        assert_eq!(style.bg_rgb, Some(0x4e5a7b));
        let css = style.inline_style();
        assert!(css.contains(&format!("color:#{:06x}", 0x0c2238)));
        assert!(css.contains(&format!("background:#{:06x}", 0x4e5a7b)));
    }

    #[test]
    fn carriage_return_preserves_the_untouched_suffix_and_backspace_only_moves_the_cursor() {
        let lines = styled_lines(
            "CR_OLD_SUFFIX\rCR_NEW\nBS_OLD\u{8}\u{8}\u{8}NEW\nretained\u{8}\u{8}\n",
            false,
        );
        let visible: Vec<String> = lines
            .iter()
            .map(|line| line.runs.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(visible, ["CR_NEW_SUFFIX", "BS_NEW", "retained"]);
    }

    #[test]
    fn cursor_overwrite_respects_wide_cells_and_combining_marks() {
        let lines = styled_lines("WIDE_界\u{8}X\nCOMBINING_e\u{301}\u{8}X\n界Z\rX\n", false);
        let visible: Vec<String> = lines
            .iter()
            .map(|line| line.runs.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert_eq!(visible, ["WIDE_ X", "COMBINING_X", "X Z"]);
    }
}
