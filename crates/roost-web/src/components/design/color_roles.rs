//! The `/design` gallery's colour-role catalog: every `--color-*` role the
//! `@theme inline` bridge in `tailwind.css` declares, grouped as the gallery
//! swatches them. `token_sections.rs` draws it; `tests/design_catalog.rs` pins
//! it against the bridge so a new role cannot go unswatched.

/// A colour role: its `--color-<name>` theme entry, and the utility that
/// paints a swatch with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRole {
    /// The role name, without the `--color-` prefix.
    pub name: &'static str,
    /// The background utility that paints the swatch (`bg-<name>`).
    pub class: &'static str,
}

/// A titled group of colour roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorGroup {
    /// The group's section title.
    pub title: &'static str,
    /// The roles swatched, in display order.
    pub roles: &'static [ColorRole],
}

/// Every theme colour role, grouped as the gallery shows them.
pub const COLOR_GROUPS: [ColorGroup; 7] = [
    ColorGroup {
        title: "Surfaces",
        roles: &[
            ColorRole {
                name: "background",
                class: "bg-background",
            },
            ColorRole {
                name: "card",
                class: "bg-card",
            },
            ColorRole {
                name: "popover",
                class: "bg-popover",
            },
            ColorRole {
                name: "muted",
                class: "bg-muted",
            },
            ColorRole {
                name: "secondary",
                class: "bg-secondary",
            },
        ],
    },
    ColorGroup {
        title: "Text",
        roles: &[
            ColorRole {
                name: "foreground",
                class: "bg-foreground",
            },
            ColorRole {
                name: "card-foreground",
                class: "bg-card-foreground",
            },
            ColorRole {
                name: "popover-foreground",
                class: "bg-popover-foreground",
            },
            ColorRole {
                name: "muted-foreground",
                class: "bg-muted-foreground",
            },
            ColorRole {
                name: "secondary-foreground",
                class: "bg-secondary-foreground",
            },
        ],
    },
    ColorGroup {
        title: "Actions",
        roles: &[
            ColorRole {
                name: "primary",
                class: "bg-primary",
            },
            ColorRole {
                name: "primary-foreground",
                class: "bg-primary-foreground",
            },
            ColorRole {
                name: "accent",
                class: "bg-accent",
            },
            ColorRole {
                name: "accent-foreground",
                class: "bg-accent-foreground",
            },
        ],
    },
    ColorGroup {
        title: "Status",
        roles: &[
            ColorRole {
                name: "destructive",
                class: "bg-destructive",
            },
            ColorRole {
                name: "destructive-foreground",
                class: "bg-destructive-foreground",
            },
            ColorRole {
                name: "success",
                class: "bg-success",
            },
            ColorRole {
                name: "success-foreground",
                class: "bg-success-foreground",
            },
            ColorRole {
                name: "warning",
                class: "bg-warning",
            },
            ColorRole {
                name: "warning-foreground",
                class: "bg-warning-foreground",
            },
            ColorRole {
                name: "info",
                class: "bg-info",
            },
            ColorRole {
                name: "info-foreground",
                class: "bg-info-foreground",
            },
        ],
    },
    ColorGroup {
        title: "Chrome",
        roles: &[
            ColorRole {
                name: "border",
                class: "bg-border",
            },
            ColorRole {
                name: "input",
                class: "bg-input",
            },
            ColorRole {
                name: "ring",
                class: "bg-ring",
            },
            ColorRole {
                name: "brand",
                class: "bg-brand",
            },
        ],
    },
    ColorGroup {
        title: "Terminal",
        roles: &[
            ColorRole {
                name: "terminal",
                class: "bg-terminal",
            },
            ColorRole {
                name: "terminal-foreground",
                class: "bg-terminal-foreground",
            },
        ],
    },
    ColorGroup {
        title: "Sidenav",
        roles: &[
            ColorRole {
                name: "sidenav",
                class: "bg-sidenav",
            },
            ColorRole {
                name: "sidenav-foreground",
                class: "bg-sidenav-foreground",
            },
            ColorRole {
                name: "sidenav-primary",
                class: "bg-sidenav-primary",
            },
            ColorRole {
                name: "sidenav-primary-foreground",
                class: "bg-sidenav-primary-foreground",
            },
            ColorRole {
                name: "sidenav-accent",
                class: "bg-sidenav-accent",
            },
            ColorRole {
                name: "sidenav-accent-foreground",
                class: "bg-sidenav-accent-foreground",
            },
            ColorRole {
                name: "sidenav-border",
                class: "bg-sidenav-border",
            },
            ColorRole {
                name: "sidenav-ring",
                class: "bg-sidenav-ring",
            },
        ],
    },
];
