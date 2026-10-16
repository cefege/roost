//! Tool output as styled lines, the way a terminal running OMP shows it: SGR
//! colours and weights kept (the 16 terminal colours, bold, dim, italic,
//! underline), other control sequences dropped, and diff lines toned as added,
//! removed or hunk header. Pure; `tool_card` renders and windows the lines.

/// Lines past this many characters are cut, so one minified line cannot
/// flood the transcript.
const MAX_LINE_CHARS: usize = 4_000;

/// The SGR attributes a run carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RunStyle {
    /// One of the 16 terminal colours (`--term-color-N`).
    pub color: Option<u8>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl RunStyle {
    /// The run's classes, empty for unstyled text.
    pub fn classes(self) -> String {
        let mut classes = Vec::new();
        let color = self.color.map(|index| format!("agent-chat__ansi-{index}"));
        classes.extend(color.as_deref());
        for (on, class) in [
            (self.bold, "agent-chat__ansi-bold"),
            (self.dim, "agent-chat__ansi-dim"),
            (self.italic, "agent-chat__ansi-italic"),
            (self.underline, "agent-chat__ansi-underline"),
        ] {
            if on {
                classes.push(class);
            }
        }
        classes.join(" ")
    }

    fn apply_sgr(&mut self, parameters: &str) {
        let codes: Vec<u16> = parameters
            .split([';', ':'])
            .map(|code| code.parse().unwrap_or(0))
            .collect();
        let mut index = 0;
        while index < codes.len() {
            match codes[index] {
                0 => *self = Self::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                22 => (self.bold, self.dim) = (false, false),
                23 => self.italic = false,
                24 => self.underline = false,
                code @ 30..=37 => self.color = u8::try_from(code - 30).ok(),
                code @ 90..=97 => self.color = u8::try_from(code - 90 + 8).ok(),
                39 => self.color = None,
                // Extended colours: only the 16-colour part of the 256 cube maps
                // onto the theme; anything else keeps the terminal ink.
                38 => match codes.get(index + 1) {
                    Some(5) => {
                        self.color = codes
                            .get(index + 2)
                            .and_then(|code| u8::try_from(*code).ok())
                            .filter(|code| *code < 16);
                        index += 2;
                    }
                    Some(2) => {
                        self.color = None;
                        index += 4;
                    }
                    _ => {}
                },
                48 => match codes.get(index + 1) {
                    Some(5) => index += 2,
                    Some(2) => index += 4,
                    _ => {}
                },
                _ => {}
            }
            index += 1;
        }
    }
}

/// One coloured run of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Run {
    pub text: String,
    pub style: RunStyle,
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
    let mut lines = Vec::new();
    let mut style = RunStyle::default();
    for raw in text.trim_end_matches('\n').split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let runs = parse_runs(raw, &mut style);
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

fn parse_runs(line: &str, style: &mut RunStyle) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut text = String::new();
    let mut length = 0;
    let mut truncated = false;
    let mut chars = line.chars().peekable();
    let flush = |runs: &mut Vec<Run>, text: &mut String, style: RunStyle| {
        if !text.is_empty() {
            runs.push(Run {
                text: std::mem::take(text),
                style,
            });
        }
    };
    while let Some(ch) = chars.next() {
        // A bare carriage return (a progress bar redrawing) starts the line
        // over, as a terminal shows it; the style it reached carries on.
        if ch == '\r' {
            runs.clear();
            text.clear();
            length = 0;
            truncated = false;
            continue;
        }
        if ch != '\u{1b}' {
            if truncated {
                continue;
            }
            if length == MAX_LINE_CHARS {
                text.push('…');
                truncated = true;
                continue;
            }
            if ch == '\t' || !ch.is_control() {
                text.push(ch);
                length += 1;
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut parameters = String::new();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        if next == 'm' {
                            flush(&mut runs, &mut text, *style);
                            style.apply_sgr(&parameters);
                        }
                        break;
                    }
                    parameters.push(next);
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    flush(&mut runs, &mut text, *style);
    runs
}

#[cfg(test)]
mod tests {
    use super::{LineTone, MAX_LINE_CHARS, RunStyle, reads_as_diff, styled_lines, window};

    #[test]
    fn sgr_colours_carry_across_lines_until_reset_and_other_escapes_vanish() {
        let lines = styled_lines(
            "\u{1b}[1;31merror\u{1b}]0;title\u{7}\nstill red\u{1b}[0m plain\n",
            false,
        );
        assert_eq!(lines.len(), 2);
        let red_bold = RunStyle {
            color: Some(1),
            bold: true,
            ..RunStyle::default()
        };
        assert_eq!(lines[0].runs[0].text, "error");
        assert_eq!(lines[0].runs[0].style, red_bold);
        assert_eq!(lines[1].runs[0].style, red_bold);
        assert_eq!(lines[1].runs[1].text, " plain");
        assert_eq!(lines[1].runs[1].style, RunStyle::default());
        assert_eq!(
            red_bold.classes(),
            "agent-chat__ansi-1 agent-chat__ansi-bold"
        );
    }

    #[test]
    fn bright_and_256_colour_codes_map_onto_the_sixteen_and_truecolour_is_dropped() {
        let lines = styled_lines(
            "\u{1b}[92ma\u{1b}[38;5;4mb\u{1b}[38;5;200mc\u{1b}[38;2;1;2;3;1md",
            false,
        );
        let colors: Vec<_> = lines[0].runs.iter().map(|run| run.style.color).collect();
        assert_eq!(colors, [Some(10), Some(4), None, None]);
        assert!(lines[0].runs[3].style.bold);
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
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "100% done");
        assert_eq!(lines[0].runs[0].style.color, Some(2));
        let tag = "1A2B";
        assert!(!reads_as_diff("write", &format!("[a.rs#{tag}]\n")));
    }

    #[test]
    fn a_line_cut_at_the_cap_still_applies_its_closing_reset() {
        let long = "x".repeat(MAX_LINE_CHARS + 50);
        let text = format!("\u{1b}[31m{long}\u{1b}[0m tail\nnext\n");
        let lines = styled_lines(&text, false);
        assert_eq!(lines.len(), 2);
        let first: String = lines[0].runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(first.matches('…').count(), 1);
        assert!(!first.contains("tail"));
        assert_eq!(lines[0].runs[0].style.color, Some(1));
        assert_eq!(lines[1].runs[0].text, "next");
        assert_eq!(lines[1].runs[0].style.color, None);
    }
}
