//! The value half of the painted-terminal probes: the render probe record, the
//! row/viewport text normalisation, the grid dimensions read, and the focus
//! record. Native; `smoke::dom` supplies the DOM values and `smoke::backdoor`
//! answers `renderProbe`/`viewportText`/`terminalDimensions`/`paneFocused`.
//! Ports `apps/web/src/smoke/smokeTerminalRenderProbes.ts:24-68,147-160`.

use serde::Serialize;

/// What `renderProbe()` reports (`SmokeRenderProbe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmokeRenderProbe {
    pub found: bool,
    /// `"cell"` when a grid is mounted, `"none"` otherwise.
    pub mode: &'static str,
    pub scroll_top: i64,
    pub scroll_height: i64,
    pub client_height: i64,
    pub from_bottom: i64,
    pub at_bottom: bool,
    pub row_count: usize,
    pub non_empty_rows: usize,
    pub first_line: String,
    pub last_line: String,
}

/// The scroll box of a mounted `.cell-grid`, as read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridScrollBox {
    pub scroll_top: f64,
    pub scroll_height: f64,
    pub client_height: f64,
}

impl SmokeRenderProbe {
    /// No grid under the slot.
    pub fn absent() -> Self {
        Self {
            found: false,
            mode: "none",
            scroll_top: 0,
            scroll_height: 0,
            client_height: 0,
            from_bottom: 0,
            at_bottom: false,
            row_count: 0,
            non_empty_rows: 0,
            first_line: String::new(),
            last_line: String::new(),
        }
    }

    /// A mounted grid: its scroll box and every `.cell-row`'s raw text.
    pub fn of_grid(scroll: GridScrollBox, raw_rows: &[String]) -> Self {
        let rows: Vec<String> = raw_rows.iter().map(|row| painted_row_text(row)).collect();
        let non_empty: Vec<&String> = rows.iter().filter(|text| !text.trim().is_empty()).collect();
        let from_bottom = scroll.scroll_height - scroll.scroll_top - scroll.client_height;
        Self {
            found: true,
            mode: "cell",
            scroll_top: js_round(scroll.scroll_top),
            scroll_height: js_round(scroll.scroll_height),
            client_height: js_round(scroll.client_height),
            from_bottom: js_round(from_bottom),
            at_bottom: scroll.scroll_top >= (scroll.scroll_height - scroll.client_height).max(0.0),
            row_count: rows.len(),
            non_empty_rows: non_empty.len(),
            first_line: non_empty.first().map(|line| (*line).clone()).unwrap_or_default(),
            last_line: non_empty.last().map(|line| (*line).clone()).unwrap_or_default(),
        }
    }
}

/// A row as the probe compares it: NBSP painted as a space, trailing blanks off.
pub fn painted_row_text(raw: &str) -> String {
    raw.replace('\u{a0}', " ").trim_end().to_owned()
}

/// A slot's text with every whitespace run collapsed (`viewportText`).
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Math.round`: halves round toward +∞.
pub fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

/// `parseInt(value, 10)` kept only when it is a safe integer, else 0 — the
/// `--cell-cols` read of `terminalDimensions`.
pub fn parse_cell_cols(style_value: &str) -> i64 {
    let trimmed = style_value.trim_start();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let run: String = digits.chars().take_while(char::is_ascii_digit).collect();
    match run.parse::<i64>() {
        Ok(value) if value <= 9_007_199_254_740_991 => sign * value,
        _ => 0,
    }
}

/// What `terminalDimensions()` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TerminalDimensions {
    pub cols: i64,
    pub rows: usize,
}

/// What `paneFocused()` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneFocus {
    pub has_slot: bool,
    pub has_textarea: bool,
    pub focused: bool,
}

/// The slot test id every pane mounts under.
pub fn terminal_slot_selector(session_id: &str) -> String {
    format!("[data-testid=\"terminal-slot-{}\"]", css_escape(session_id))
}

/// `CSS.escape` for an attribute value inside double quotes: the characters
/// that could end or corrupt the selector are escaped.
pub fn css_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '"' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
