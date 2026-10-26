//! The `/design` gallery's scale catalogs: the type, radius, spacing,
//! elevation, status and grid specimens it draws (the colour roles are
//! `color_roles.rs`). Every class is a complete utility literal so Tailwind
//! scans it; `tests/design_catalog.rs` pins the radius names against the
//! `@theme inline` block in `tailwind.css`.

/// One specimen: the caption shown beside it and the classes that paint it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Specimen {
    /// The utility name shown as the caption.
    pub label: &'static str,
    /// The complete class string that draws the specimen.
    pub class: &'static str,
}

/// The type scale, largest first; each sample is set in its own size and weight.
pub const TYPE_STEPS: [Specimen; 8] = [
    Specimen {
        label: "text-4xl",
        class: "text-4xl font-bold",
    },
    Specimen {
        label: "text-3xl",
        class: "text-3xl font-bold",
    },
    Specimen {
        label: "text-2xl",
        class: "text-2xl font-semibold",
    },
    Specimen {
        label: "text-xl",
        class: "text-xl font-semibold",
    },
    Specimen {
        label: "text-lg",
        class: "text-lg font-semibold",
    },
    Specimen {
        label: "text-base",
        class: "text-base font-medium",
    },
    Specimen {
        label: "text-sm",
        class: "text-sm font-normal",
    },
    Specimen {
        label: "text-xs",
        class: "text-xs font-normal",
    },
];

/// The corner-radius scale, one `rounded-*` per `--radius-*` theme entry.
pub const SHAPE_STEPS: [Specimen; 12] = [
    Specimen {
        label: "rounded-xs",
        class: "size-12 rounded-xs border border-border bg-accent",
    },
    Specimen {
        label: "rounded-sm",
        class: "size-12 rounded-sm border border-border bg-accent",
    },
    Specimen {
        label: "rounded-md",
        class: "size-12 rounded-md border border-border bg-accent",
    },
    Specimen {
        label: "rounded-lg",
        class: "size-12 rounded-lg border border-border bg-accent",
    },
    Specimen {
        label: "rounded-xl",
        class: "size-12 rounded-xl border border-border bg-accent",
    },
    Specimen {
        label: "rounded-2xl",
        class: "size-12 rounded-2xl border border-border bg-accent",
    },
    Specimen {
        label: "rounded-row",
        class: "size-12 rounded-row border border-border bg-accent",
    },
    Specimen {
        label: "rounded-control",
        class: "size-12 rounded-control border border-border bg-accent",
    },
    Specimen {
        label: "rounded-card",
        class: "size-12 rounded-card border border-border bg-accent",
    },
    Specimen {
        label: "rounded-window",
        class: "size-12 rounded-window border border-border bg-accent",
    },
    Specimen {
        label: "rounded-sheet",
        class: "size-12 rounded-sheet border border-border bg-accent",
    },
    Specimen {
        label: "rounded-pill",
        class: "size-12 rounded-pill border border-border bg-accent",
    },
];

/// The spacing scale as bars of each width.
pub const SPACE_BARS: [Specimen; 9] = [
    Specimen {
        label: "w-1",
        class: "h-4 w-1 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-2",
        class: "h-4 w-2 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-3",
        class: "h-4 w-3 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-4",
        class: "h-4 w-4 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-5",
        class: "h-4 w-5 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-6",
        class: "h-4 w-6 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-8",
        class: "h-4 w-8 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-10",
        class: "h-4 w-10 rounded-xs bg-primary",
    },
    Specimen {
        label: "w-12",
        class: "h-4 w-12 rounded-xs bg-primary",
    },
];

/// The elevation scale, one `shadow-*` per step.
pub const ELEVATION_STEPS: [Specimen; 8] = [
    Specimen {
        label: "shadow-xs",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-xs",
    },
    Specimen {
        label: "shadow-sm",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-sm",
    },
    Specimen {
        label: "shadow-md",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-md",
    },
    Specimen {
        label: "shadow-lg",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-lg",
    },
    Specimen {
        label: "shadow-xl",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-xl",
    },
    Specimen {
        label: "shadow-2xl",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-2xl",
    },
    Specimen {
        label: "shadow-card",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-card",
    },
    Specimen {
        label: "shadow-pop",
        class: "flex h-16 items-center justify-center rounded-md bg-card font-mono text-xs text-muted-foreground shadow-pop",
    },
];

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
    DenseGridRow {
        name: "apps",
        support: "2h ago",
        selected: false,
    },
    DenseGridRow {
        name: "docs",
        support: "just now",
        selected: true,
    },
    DenseGridRow {
        name: "packages",
        support: "5m ago",
        selected: false,
    },
    DenseGridRow {
        name: "scripts",
        support: "3d ago",
        selected: false,
    },
    DenseGridRow {
        name: "smoke",
        support: "1h ago",
        selected: false,
    },
    DenseGridRow {
        name: "very-long-folder-name-that-must-ellipsize",
        support: "12d ago",
        selected: false,
    },
];

/// The swatch grid: columns at least a 9rem wide, filling the row.
pub const SWATCH_GRID_CLASS: &str = "grid grid-cols-[repeat(auto-fill,minmax(9rem,1fr))] gap-4";

/// The specimen card grid: columns at least a 15rem wide.
pub const CARD_GRID_CLASS: &str = "grid grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] gap-4 mb-5";

/// The metric and surface grid: columns at least a 12rem wide.
pub const TILE_GRID_CLASS: &str = "grid grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] gap-4 mb-5";
