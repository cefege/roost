//! The `/design` gallery's catalogs: which tokens it swatches, which steps of
//! each scale it draws, and the specimen rows it lists. Ported from the
//! constants at the top of `apps/web/src/components/design/DesignGallery.tsx`;
//! read by the gallery's sections. Every token named here maps 1:1 to a custom
//! property declared in `assets/styles/theme-vars.css`.

/// A titled group of colour tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorGroup {
    /// The group's section title.
    pub title: &'static str,
    /// The custom properties swatched, with their `--`.
    pub tokens: &'static [&'static str],
}

/// The colour roles, grouped as the gallery shows them.
pub const COLOR_GROUPS: [ColorGroup; 6] = [
    ColorGroup {
        title: "Surfaces",
        tokens: &["--surface-0", "--surface-1", "--surface-2", "--surface-3", "--bg-base", "--term-bg"],
    },
    ColorGroup {
        title: "Text",
        tokens: &["--text-hi", "--text-mid", "--text-lo"],
    },
    ColorGroup {
        title: "Accent",
        tokens: &["--accent", "--brand-coral", "--accent-container", "--on-accent"],
    },
    ColorGroup {
        title: "M3 roles",
        tokens: &[
            "--md-primary",
            "--md-primary-container",
            "--md-secondary-container",
            "--md-surface-container",
            "--md-surface-container-high",
            "--md-outline",
            "--md-outline-variant",
        ],
    },
    ColorGroup {
        title: "Status",
        tokens: &["--status-ok", "--status-warn", "--status-err", "--status-info"],
    },
    ColorGroup {
        title: "ANSI",
        tokens: &[
            "--ansi-black",
            "--ansi-red",
            "--ansi-green",
            "--ansi-yellow",
            "--ansi-blue",
            "--ansi-magenta",
            "--ansi-cyan",
            "--ansi-white",
            "--ansi-bright-black",
            "--ansi-bright-red",
            "--ansi-bright-green",
            "--ansi-bright-yellow",
            "--ansi-bright-blue",
            "--ansi-bright-magenta",
            "--ansi-bright-cyan",
            "--ansi-bright-white",
        ],
    },
];

/// The type ramp, largest first; each step has `-size`, `-line` and `-weight`.
pub const RAMP_STEPS: [&str; 15] = [
    "display-l",
    "display-m",
    "display-s",
    "headline-l",
    "headline-m",
    "headline-s",
    "title-l",
    "title-m",
    "title-s",
    "body-l",
    "body-m",
    "body-s",
    "label-l",
    "label-m",
    "label-s",
];

/// The `--md-space-N` steps.
pub const SPACE_STEPS: [u8; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 9];

/// The `--md-shape-*` steps.
pub const SHAPE_STEPS: [&str; 6] = ["xs", "sm", "md", "lg", "xl", "full"];

/// The `--md-elev-N` steps.
pub const ELEV_STEPS: [u8; 6] = [0, 1, 2, 3, 4, 5];

/// The statuses the dot specimen shows, solid and hollow.
pub const STATUS_DOTS: [&str; 5] = ["ok", "running", "idle", "error", "offline"];

/// The terminal stream indicator's three states, compared side by side.
pub const STREAM_INDICATOR_STATES: [&str; 3] = ["receiving", "catching_up", "detached"];

/// One row of the dense-grid list specimen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenseGridRow {
    /// The folder name; the last one must ellipsize.
    pub name: &'static str,
    /// The relative-time support line.
    pub support: &'static str,
    /// Whether the row is drawn selected.
    pub selected: bool,
}

/// The dense-grid specimen's rows.
pub const DENSE_GRID_ROWS: [DenseGridRow; 6] = [
    DenseGridRow { name: "apps", support: "2h ago", selected: false },
    DenseGridRow { name: "docs", support: "just now", selected: true },
    DenseGridRow { name: "packages", support: "5m ago", selected: false },
    DenseGridRow { name: "scripts", support: "3d ago", selected: false },
    DenseGridRow { name: "smoke", support: "1h ago", selected: false },
    DenseGridRow {
        name: "very-long-folder-name-that-must-ellipsize",
        support: "12d ago",
        selected: false,
    },
];

/// A responsive grid whose columns are at least `min` wide.
pub fn gallery_grid_style(min: &str) -> String {
    format!(
        "display: grid; grid-template-columns: repeat(auto-fill, minmax({min}, 1fr)); \
         gap: var(--md-space-4);"
    )
}
