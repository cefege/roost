//! The design-system primitives every surface composes: panels, buttons, rows,
//! form controls, status dots and modals. Ported from
//! `apps/web/src/components/Settings/md/primitives.tsx` (the barrel) and the
//! twenty-one sibling modules it re-exported; the `/design` gallery renders each.
//!
//! Compose from these; never hand-roll a styled `div` panel, a `<button>`, or a
//! coloured status span beside them. Class names, ARIA and `data-testid`s are
//! v2's exactly, because `controls.css`, `tokens.css`, `overlays.css` and the
//! Playwright specs select on them. Each primitive keeps its rules in a pure
//! function beside it so the native tests reach them without a document.

pub mod binding_chip;
pub mod button;
pub mod card;
pub mod checkbox;
pub mod chip;
pub mod class_list;
pub mod dialog;
pub mod dom;
pub mod empty_state;
pub mod focus_scope;
pub mod form_field;
pub mod icon;
pub mod icon_button;
pub mod list;
pub mod list_row;
pub mod metric_tile;
pub mod section_title;
pub mod select;
pub mod select_listbox;
pub mod select_navigation;
pub mod select_placement;
pub mod sheet;
pub mod skeleton;
pub mod status_dot;
pub mod stylesheet;
pub mod surface;
pub mod switch;
pub mod switch_row;
pub mod text_field;

pub use binding_chip::BindingChip;
pub use button::{Button, ButtonSize, ButtonVariant};
pub use card::{Card, CardVariant};
pub use checkbox::Checkbox;
pub use chip::Chip;
pub use dialog::Dialog;
pub use empty_state::EmptyState;
pub use focus_scope::AutoFocusRequest;
pub use icon::{Icon, IconSize};
pub use icon_button::{IconButton, IconButtonSize};
pub use list::{List, ListLayout};
pub use list_row::ListRow;
pub use metric_tile::MetricTile;
pub use section_title::SectionTitle;
pub use select::{Select, SelectOption};
pub use sheet::{Sheet, SheetSide};
pub use skeleton::Skeleton;
pub use status_dot::StatusDot;
pub use surface::{Surface, SurfaceElement, SurfaceRadius};
pub use switch::Switch;
pub use switch_row::SwitchRow;
pub use text_field::TextField;
