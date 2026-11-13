//! ANSI rendition state shared by native rows and tool output.
//! SGR colors and flags use the terminal grid's palette and CSS conversion.
//! Parsing never chooses application colors or rewrites producer text.

use roost_protocol::cell::{
    CELL_BLINK, CELL_BOLD, CELL_DIM, CELL_INVISIBLE, CELL_ITALIC, CELL_REVERSE, CELL_STRIKE,
    CELL_UNDERLINE, CellSpan, DEFAULT_COLOR,
};
use roost_web_terminal::cell_row::{ansi256_to_css, span_style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunStyle {
    pub fg: u16,
    pub bg: u16,
    pub flags: u16,
    pub fg_rgb: Option<u32>,
    pub bg_rgb: Option<u32>,
    underline_style: u8,
    underline_color: Option<(u16, Option<u32>)>,
    overline: bool,
}

impl Default for RunStyle {
    fn default() -> Self {
        Self {
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            underline_style: 0,
            underline_color: None,
            overline: false,
        }
    }
}

impl RunStyle {
    pub fn inline_style(self) -> String {
        // No cell occupancy is projected: these are flowing text runs, not grid cells.
        let mut style = span_style(&CellSpan {
            text: String::new(),
            fg: self.fg,
            bg: self.bg,
            flags: self.flags,
            fg_rgb: self.fg_rgb,
            bg_rgb: self.bg_rgb,
            columns: 0,
            link_uri: None,
            link_key: None,
        });
        if self.fg == DEFAULT_COLOR && self.fg_rgb.is_none() && self.flags & CELL_REVERSE == 0 {
            // Uncolored runs inherit diff/semantic tones from their container.
            let inherited = "color:var(--term-fg)";
            if style.starts_with(inherited) {
                let length = inherited.len()
                    + usize::from(style.as_bytes().get(inherited.len()) == Some(&b';'));
                drop(style.drain(..length));
            }
        }
        if self.overline {
            style.push_str(";text-decoration-line:overline");
            if self.flags & CELL_UNDERLINE != 0 {
                style.push_str(" underline");
            }
            if self.flags & CELL_STRIKE != 0 {
                style.push_str(" line-through");
            }
        }
        if let Some(decoration) = match self.underline_style {
            2 => Some("double"),
            3 => Some("wavy"),
            4 => Some("dotted"),
            5 => Some("dashed"),
            _ => None,
        } {
            style.push_str(";text-decoration-style:");
            style.push_str(decoration);
        }
        if let Some((index, rgb)) = self.underline_color {
            style.push_str(";text-decoration-color:");
            style.push_str(&rgb.map_or_else(|| ansi256_to_css(index), |rgb| format!("#{rgb:06x}")));
        }
        style
    }

    pub(super) fn apply_sgr(&mut self, parameters: &str) {
        let mut parameters = parameters.split(';');
        while let Some(parameter) = parameters.next() {
            if parameter.contains(':') {
                self.apply_colon(parameter);
                continue;
            }
            let Some(code) = number(parameter) else {
                continue;
            };
            match code {
                0 => *self = Self::default(),
                1 => self.flags |= CELL_BOLD,
                2 => self.flags |= CELL_DIM,
                3 => self.flags |= CELL_ITALIC,
                4 | 21 => {
                    self.flags |= CELL_UNDERLINE;
                    self.underline_style = if code == 21 { 2 } else { 1 };
                }
                5 | 6 => self.flags |= CELL_BLINK,
                7 => self.flags |= CELL_REVERSE,
                8 => self.flags |= CELL_INVISIBLE,
                9 => self.flags |= CELL_STRIKE,
                22 => self.flags &= !(CELL_BOLD | CELL_DIM),
                23 => self.flags &= !CELL_ITALIC,
                24 => {
                    self.flags &= !CELL_UNDERLINE;
                    self.underline_style = 0;
                }
                25 => self.flags &= !CELL_BLINK,
                27 => self.flags &= !CELL_REVERSE,
                28 => self.flags &= !CELL_INVISIBLE,
                29 => self.flags &= !CELL_STRIKE,
                30..=37 => {
                    self.fg = code - 30;
                    self.fg_rgb = None;
                }
                40..=47 => {
                    self.bg = code - 40;
                    self.bg_rgb = None;
                }
                90..=97 => {
                    self.fg = code - 90 + 8;
                    self.fg_rgb = None;
                }
                100..=107 => {
                    self.bg = code - 100 + 8;
                    self.bg_rgb = None;
                }
                38 | 48 | 58 => {
                    if let Some(color) = extended_color(&mut parameters) {
                        self.set_color(code, color);
                    }
                }
                39 => {
                    self.fg = DEFAULT_COLOR;
                    self.fg_rgb = None;
                }
                49 => {
                    self.bg = DEFAULT_COLOR;
                    self.bg_rgb = None;
                }
                53 => self.overline = true,
                55 => self.overline = false,
                59 => self.underline_color = None,
                _ => {}
            }
        }
    }

    fn apply_colon(&mut self, parameter: &str) {
        let mut parts = parameter.split(':');
        let Some(code) = parts.next().and_then(number) else {
            return;
        };
        if code == 4 {
            if let Some(style @ 0..=5) = parts.next().and_then(number) {
                self.underline_style = style as u8;
                if style == 0 {
                    self.flags &= !CELL_UNDERLINE;
                } else {
                    self.flags |= CELL_UNDERLINE;
                }
            }
            return;
        }
        if !matches!(code, 38 | 48 | 58) {
            return;
        }
        let mode = parts.next().and_then(number);
        let color = match mode {
            Some(5) => parts
                .next()
                .and_then(number)
                .filter(|index| *index <= 255)
                .map(|index| (index, None)),
            Some(2) => {
                let mut channels = [0_u16; 4];
                let mut count = 0;
                for part in parts {
                    if count == channels.len() {
                        return;
                    }
                    let Some(channel @ 0..=255) = number(part) else {
                        return;
                    };
                    channels[count] = channel;
                    count += 1;
                }
                match count {
                    3 => Some(truecolor(channels[0], channels[1], channels[2])),
                    4 => Some(truecolor(channels[1], channels[2], channels[3])),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(color) = color {
            self.set_color(code, color);
        }
    }

    fn set_color(&mut self, code: u16, color: (u16, Option<u32>)) {
        match code {
            38 => (self.fg, self.fg_rgb) = color,
            48 => (self.bg, self.bg_rgb) = color,
            58 => self.underline_color = Some(color),
            _ => {}
        }
    }
}

fn number(value: &str) -> Option<u16> {
    if value.is_empty() {
        Some(0)
    } else {
        value.parse().ok()
    }
}

fn truecolor(red: u16, green: u16, blue: u16) -> (u16, Option<u32>) {
    (
        DEFAULT_COLOR,
        Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)),
    )
}

fn extended_color<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Option<(u16, Option<u32>)> {
    match parts.next().and_then(number)? {
        5 => Some((
            parts
                .next()
                .and_then(number)
                .filter(|index| *index <= 255)?,
            None,
        )),
        2 => {
            let red = parts.next().and_then(number)?;
            let green = parts.next().and_then(number)?;
            let blue = parts.next().and_then(number)?;
            (red <= 255 && green <= 255 && blue <= 255).then(|| truecolor(red, green, blue))
        }
        _ => None,
    }
}
