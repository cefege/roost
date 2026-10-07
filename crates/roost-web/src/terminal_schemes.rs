//! Terminal-only palettes and import codecs for Windows Terminal, iTerm2,
//! kitty, and Ghostty. App chrome themes do not consume these palettes.
//!
//! The parsers return typed values independently of the Settings component.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
mod presets;
pub use presets::built_in_schemes;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// CSS spelling used by the terminal palette bridge.
    pub fn css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// The 16 ANSI colors and terminal-special colors consumed by `.wterm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalPalette {
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
}

/// A named scheme, suitable for built-ins or user imports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScheme {
    /// Stable selection key.
    pub id: String,
    /// Label shown in Settings.
    pub name: String,
    /// The terminal palette.
    pub palette: TerminalPalette,
}

/// Format-specific import errors. Values are not echoed, avoiding terminal
/// configuration text in diagnostics or user-facing error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemeParseError {
    InvalidJson,
    InvalidFormat,
    MissingField(&'static str),
    InvalidColor(&'static str),
}

impl fmt::Display for SchemeParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("This is not valid Windows Terminal JSON."),
            Self::InvalidFormat => {
                formatter.write_str("The color scheme format was not recognized.")
            }
            Self::MissingField(field) => write!(formatter, "The scheme is missing {field}."),
            Self::InvalidColor(field) => write!(formatter, "The {field} color is invalid."),
        }
    }
}

impl std::error::Error for SchemeParseError {}

/// Parse one supported portable scheme format, identified by its contents.
pub fn parse_scheme(source: &str) -> Result<TerminalScheme, SchemeParseError> {
    let trimmed = source.trim();
    if trimmed.starts_with('{') {
        return parse_windows_terminal(trimmed);
    }
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<plist") {
        return parse_iterm(trimmed);
    }
    parse_key_value(trimmed)
}

fn parse_windows_terminal(source: &str) -> Result<TerminalScheme, SchemeParseError> {
    let value: Value = serde_json::from_str(source).map_err(|_| SchemeParseError::InvalidJson)?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported");
    let ansi = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "purple",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightPurple",
        "brightCyan",
        "brightWhite",
    ];
    let mut colors = [Rgb(0, 0, 0); 16];
    for (index, field) in ansi.iter().enumerate() {
        colors[index] = json_color(&value, field)?;
    }
    let foreground = json_color(&value, "foreground")?;
    let background = json_color(&value, "background")?;
    let cursor = json_color_or(&value, "cursorColor", foreground);
    let cursor_text = json_color_or(&value, "cursorText", background);
    let selection_background = json_color_or(&value, "selectionBackground", colors[8]);
    let selection_foreground = json_color_or(&value, "selectionForeground", foreground);
    Ok(TerminalScheme {
        name: name.to_owned(),
        id: String::new(),
        palette: TerminalPalette {
            ansi: colors,
            foreground,
            background,
            cursor,
            cursor_text,
            selection_background,
            selection_foreground,
        },
    })
}

fn json_color(value: &Value, field: &'static str) -> Result<Rgb, SchemeParseError> {
    let raw = value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(SchemeParseError::MissingField(field))?;
    parse_color(raw).ok_or(SchemeParseError::InvalidColor(field))
}

fn json_color_or(value: &Value, field: &'static str, fallback: Rgb) -> Rgb {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_color)
        .unwrap_or(fallback)
}

fn parse_iterm(source: &str) -> Result<TerminalScheme, SchemeParseError> {
    let mut values = std::collections::BTreeMap::<String, String>::new();
    let mut current_color = None::<String>;
    let mut cursor = source;
    while let Some(key_start) = cursor.find("<key>") {
        cursor = &cursor[key_start + 5..];
        let key_end = cursor
            .find("</key>")
            .ok_or(SchemeParseError::InvalidFormat)?;
        let key = xml_unescape(&cursor[..key_end]);
        cursor = &cursor[key_end + 6..];
        let value_start = cursor.find('<').ok_or(SchemeParseError::InvalidFormat)?;
        let tag_end = cursor[value_start..]
            .find('>')
            .ok_or(SchemeParseError::InvalidFormat)?
            + value_start;
        let tag = &cursor[value_start + 1..tag_end];
        if tag == "dict" {
            if key.ends_with(" Color") {
                current_color = Some(key);
            }
            cursor = &cursor[tag_end + 1..];
            continue;
        }
        let close = format!("</{tag}>");
        let value_end = cursor[tag_end + 1..]
            .find(&close)
            .ok_or(SchemeParseError::InvalidFormat)?
            + tag_end
            + 1;
        if matches!(tag, "real" | "integer" | "string") {
            let qualified = match current_color.as_deref() {
                Some(parent) if key.ends_with(" Component") => format!("{parent} {key}"),
                _ => key,
            };
            values.insert(
                qualified,
                xml_unescape(cursor[tag_end + 1..value_end].trim()),
            );
        }
        cursor = &cursor[value_end + close.len()..];
    }
    let mut ansi = [Rgb(0, 0, 0); 16];
    for (index, color) in ansi.iter_mut().enumerate() {
        *color = iterm_color(&values, &format!("Ansi {index} Color"))?;
    }
    let foreground = iterm_color(&values, "Foreground Color")?;
    let background = iterm_color(&values, "Background Color")?;
    let cursor = iterm_color(&values, "Cursor Color").unwrap_or(foreground);
    let cursor_text = iterm_color(&values, "Cursor Text Color").unwrap_or(background);
    let selection_background = iterm_color(&values, "Selection Color").unwrap_or(ansi[8]);
    let selection_foreground = iterm_color(&values, "Selected Text Color").unwrap_or(foreground);
    Ok(TerminalScheme {
        id: String::new(),
        name: "Imported iTerm2 scheme".to_owned(),
        palette: TerminalPalette {
            ansi,
            foreground,
            background,
            cursor,
            cursor_text,
            selection_background,
            selection_foreground,
        },
    })
}

fn iterm_color(
    values: &std::collections::BTreeMap<String, String>,
    prefix: &str,
) -> Result<Rgb, SchemeParseError> {
    let read = |channel: &str| -> Result<u8, SchemeParseError> {
        let key = format!("{prefix} {channel} Component");
        let value = values
            .get(&key)
            .ok_or(SchemeParseError::MissingField("RGB component"))?;
        let unit = value
            .parse::<f64>()
            .map_err(|_| SchemeParseError::InvalidColor("RGB component"))?;
        if !(0.0..=1.0).contains(&unit) {
            return Err(SchemeParseError::InvalidColor("RGB component"));
        }
        Ok((unit * 255.0).round() as u8)
    };
    Ok(Rgb(read("Red")?, read("Green")?, read("Blue")?))
}

fn parse_key_value(source: &str) -> Result<TerminalScheme, SchemeParseError> {
    let mut values = std::collections::BTreeMap::<String, String>::new();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .or_else(|| line.split_once(char::is_whitespace))
            .ok_or(SchemeParseError::InvalidFormat)?;
        let key = key.trim().to_ascii_lowercase().replace('-', "_");
        let value = value.trim().trim_start_matches('=').trim();
        if key == "palette" {
            let (index, color) = value
                .split_once('=')
                .ok_or(SchemeParseError::InvalidFormat)?;
            let index = index
                .trim()
                .parse::<usize>()
                .map_err(|_| SchemeParseError::InvalidFormat)?;
            if index >= 16 {
                return Err(SchemeParseError::InvalidFormat);
            }
            values.insert(format!("color{index}"), color.trim().to_owned());
        } else {
            values.insert(key, value.to_owned());
        }
    }
    let read = |field: &'static str| -> Result<Rgb, SchemeParseError> {
        let value = values
            .get(field)
            .ok_or(SchemeParseError::MissingField(field))?;
        parse_color(value).ok_or(SchemeParseError::InvalidColor(field))
    };
    let mut ansi = [Rgb(0, 0, 0); 16];
    for (index, color) in ansi.iter_mut().enumerate() {
        let key = format!("color{index}");
        let value = values
            .get(&key)
            .ok_or(SchemeParseError::MissingField("ANSI colors 0–15"))?;
        *color = parse_color(value).ok_or(SchemeParseError::InvalidColor("ANSI color"))?;
    }
    let foreground = read("foreground")?;
    let background = read("background")?;
    let cursor = values
        .get("cursor_color")
        .or_else(|| values.get("cursor"))
        .and_then(|value| parse_color(value))
        .unwrap_or(foreground);
    let cursor_text = values
        .get("cursor_text_color")
        .or_else(|| values.get("cursor_text"))
        .and_then(|value| parse_color(value))
        .unwrap_or(background);
    let selection_background = values
        .get("selection_background")
        .and_then(|value| parse_color(value))
        .unwrap_or(ansi[8]);
    let selection_foreground = values
        .get("selection_foreground")
        .and_then(|value| parse_color(value))
        .unwrap_or(foreground);
    Ok(TerminalScheme {
        id: String::new(),
        name: values
            .get("name")
            .cloned()
            .unwrap_or_else(|| "Imported scheme".to_owned()),
        palette: TerminalPalette {
            ansi,
            foreground,
            background,
            cursor,
            cursor_text,
            selection_background,
            selection_foreground,
        },
    })
}

fn parse_color(value: &str) -> Option<Rgb> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let packed = u32::from_str_radix(value, 16).ok()?;
    Some(Rgb((packed >> 16) as u8, (packed >> 8) as u8, packed as u8))
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
