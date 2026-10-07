//! Published terminal palette presets, separate from the app chrome registry.
//! The registry is consumed by terminal Settings for selection and previews.
//! Palette values here never affect the Light/Dark/System app chrome.

use std::sync::LazyLock;

use super::{Rgb, TerminalPalette, TerminalScheme, parse_color};

static PRESETS: LazyLock<Vec<TerminalScheme>> = LazyLock::new(create_presets);

/// Nine named, terminal-only preset palettes.
pub fn built_in_schemes() -> &'static [TerminalScheme] {
    PRESETS.as_slice()
}

fn create_presets() -> Vec<TerminalScheme> {
    [
        (
            "Solarized Light",
            "fdf6e3",
            "657b83",
            [
                "073642", "dc322f", "859900", "b58900", "268bd2", "d33682", "2aa198", "eee8d5",
                "002b36", "cb4b16", "586e75", "657b83", "839496", "6c71c4", "93a1a1", "fdf6e3",
            ],
        ),
        (
            "Solarized Dark",
            "002b36",
            "839496",
            [
                "073642", "dc322f", "859900", "b58900", "268bd2", "d33682", "2aa198", "eee8d5",
                "002b36", "cb4b16", "586e75", "657b83", "839496", "6c71c4", "93a1a1", "fdf6e3",
            ],
        ),
        (
            "Dracula",
            "282a36",
            "f8f8f2",
            [
                "21222c", "ff5555", "50fa7b", "f1fa8c", "bd93f9", "ff79c6", "8be9fd", "f8f8f2",
                "6272a4", "ff6e6e", "69ff94", "ffffa5", "d6acff", "ff92df", "a4ffff", "ffffff",
            ],
        ),
        (
            "Gruvbox Dark",
            "282828",
            "ebdbb2",
            [
                "282828", "cc241d", "98971a", "d79921", "458588", "b16286", "689d6a", "a89984",
                "928374", "fb4934", "b8bb26", "fabd2f", "83a598", "d3869b", "8ec07c", "ebdbb2",
            ],
        ),
        (
            "Nord",
            "2e3440",
            "d8dee9",
            [
                "3b4252", "bf616a", "a3be8c", "ebcb8b", "81a1c1", "b48ead", "88c0d0", "e5e9f0",
                "4c566a", "bf616a", "a3be8c", "ebcb8b", "81a1c1", "b48ead", "8fbcbb", "eceff4",
            ],
        ),
        (
            "Catppuccin Latte",
            "eff1f5",
            "4c4f69",
            [
                "5c5f77", "d20f39", "40a02b", "df8e1d", "1e66f5", "ea76cb", "179299", "acb0be",
                "6c6f85", "d20f39", "40a02b", "df8e1d", "1e66f5", "ea76cb", "179299", "bcc0cc",
            ],
        ),
        (
            "Catppuccin Mocha",
            "1e1e2e",
            "cdd6f4",
            [
                "45475a", "f38ba8", "a6e3a1", "f9e2af", "89b4fa", "f5c2e7", "94e2d5", "bac2de",
                "585b70", "f38ba8", "a6e3a1", "f9e2af", "89b4fa", "f5c2e7", "94e2d5", "a6adc8",
            ],
        ),
        (
            "Tokyo Night",
            "1a1b26",
            "c0caf5",
            [
                "15161e", "f7768e", "9ece6a", "e0af68", "7aa2f7", "bb9af7", "7dcfff", "a9b1d6",
                "414868", "f7768e", "9ece6a", "e0af68", "7aa2f7", "bb9af7", "7dcfff", "c0caf5",
            ],
        ),
        (
            "One Dark",
            "282c34",
            "abb2bf",
            [
                "1e2127", "e06c75", "98c379", "e5c07b", "61afef", "c678dd", "56b6c2", "abb2bf",
                "5c6370", "e06c75", "98c379", "e5c07b", "61afef", "c678dd", "56b6c2", "ffffff",
            ],
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, background, foreground, ansi))| {
        let ansi = ansi.map(|hex| parse_color(hex).unwrap_or(Rgb(0, 0, 0)));
        let background = parse_color(background).unwrap_or(Rgb(0, 0, 0));
        let foreground = parse_color(foreground).unwrap_or(Rgb(0, 0, 0));
        TerminalScheme {
            id: format!("preset-{index}"),
            name: name.to_owned(),
            palette: TerminalPalette {
                ansi,
                foreground,
                background,
                cursor: foreground,
                cursor_text: background,
                selection_background: ansi[8],
                selection_foreground: foreground,
            },
        }
    })
    .collect()
}
