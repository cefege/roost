//! Converted accent color seeds from the persisted OKLCH palette catalog.

use super::super::catalog::{AccentColors, AccentThemeSpec};

/// Accent overrides in the original picker order.
pub const ACCENT_THEMES: [AccentThemeSpec; 18] = [
    AccentThemeSpec {
        id: "default",
        label: "Default",
        swatch: "#d4d4d4",
        light: AccentColors {
            primary: None,
            secondary: None,
        },
        dark: AccentColors {
            primary: None,
            secondary: None,
        },
    },
    AccentThemeSpec {
        id: "amber",
        label: "Amber",
        swatch: "#f59e0b",
        light: AccentColors {
            primary: Some(0xBB4D00),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x973C00),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "blue",
        label: "Blue",
        swatch: "#3b82f6",
        light: AccentColors {
            primary: Some(0x1447E6),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x193CB8),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "cyan",
        label: "Cyan",
        swatch: "#06b6d4",
        light: AccentColors {
            primary: Some(0x007595),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x005F78),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "emerald",
        label: "Emerald",
        swatch: "#10b981",
        light: AccentColors {
            primary: Some(0x007A55),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x006045),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "fuchsia",
        label: "Fuchsia",
        swatch: "#d946ef",
        light: AccentColors {
            primary: Some(0xA800B7),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x8A0194),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "green",
        label: "Green",
        swatch: "#22c55e",
        light: AccentColors {
            primary: Some(0x497D00),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x3C6300),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "indigo",
        label: "Indigo",
        swatch: "#6366f1",
        light: AccentColors {
            primary: Some(0x432DD7),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x372AAC),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "lime",
        label: "Lime",
        swatch: "#84cc16",
        light: AccentColors {
            primary: Some(0x497D00),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x3C6300),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "orange",
        label: "Orange",
        swatch: "#f97316",
        light: AccentColors {
            primary: Some(0xCA3500),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x9F2D00),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "pink",
        label: "Pink",
        swatch: "#ec4899",
        light: AccentColors {
            primary: Some(0xC6005C),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0xA3004C),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "purple",
        label: "Purple",
        swatch: "#a855f7",
        light: AccentColors {
            primary: Some(0x8200DB),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x6E11B0),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "red",
        label: "Red",
        swatch: "#ef4444",
        light: AccentColors {
            primary: Some(0xC10007),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x9F0712),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "rose",
        label: "Rose",
        swatch: "#f43f5e",
        light: AccentColors {
            primary: Some(0xC70036),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0xA50036),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "sky",
        label: "Sky",
        swatch: "#0ea5e9",
        light: AccentColors {
            primary: Some(0x0069A8),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x00598A),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "teal",
        label: "Teal",
        swatch: "#14b8a6",
        light: AccentColors {
            primary: Some(0x00786F),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x005F5A),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "violet",
        label: "Violet",
        swatch: "#8b5cf6",
        light: AccentColors {
            primary: Some(0x7008E7),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0x5D0EC0),
            secondary: Some(0x27272A),
        },
    },
    AccentThemeSpec {
        id: "yellow",
        label: "Yellow",
        swatch: "#eab308",
        light: AccentColors {
            primary: Some(0xFDC700),
            secondary: Some(0xF4F4F5),
        },
        dark: AccentColors {
            primary: Some(0xF0B100),
            secondary: Some(0x27272A),
        },
    },
];
