//! Directional (D-pad) focus navigation, the geometry and the gate: which
//! arrow presses spatial navigation may claim, and which control an arrow lands
//! on. A TV remote and a game controller both land here; Chromium's own spatial
//! navigation is not guaranteed on TV browsers, so without this the four
//! arrows would never move DOM focus at all.
//!
//! Pure over [`NavRect`]s and plain facts; the keydown listener that gathers
//! them is `spatial_dom`. Ported from `apps/web/src/lib/spatialNavigation.ts`.

use roost_web_terminal::reader_intent::ScrollBoxGeometry;

/// One of the four arrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// The direction a `KeyboardEvent.key` names, if it is an arrow.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "ArrowUp" => Some(Self::Up),
            "ArrowDown" => Some(Self::Down),
            "ArrowLeft" => Some(Self::Left),
            "ArrowRight" => Some(Self::Right),
            _ => None,
        }
    }
}

/// A `getBoundingClientRect()` in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NavRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl NavRect {
    /// A rect from its origin and size.
    pub fn new(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self { left, top, width, height }
    }

    fn right(&self) -> f64 {
        self.left + self.width
    }

    fn bottom(&self) -> f64 {
        self.top + self.height
    }

    fn center_x(&self) -> f64 {
        self.left + self.width / 2.0
    }

    fn center_y(&self) -> f64 {
        self.top + self.height / 2.0
    }
}

/// Off-axis drift is penalised twice as hard as distance along the travel
/// axis, so a control almost straight ahead beats a nearer one far to the side.
pub const CROSS_AXIS_WEIGHT: f64 = 2.0;

/// The index of the nearest candidate in `direction`, or `None` when nothing
/// lies that way. A candidate whose centre is not strictly beyond the origin's
/// leading edge is not a move and is rejected.
pub fn best_candidate_in_direction(
    from: &NavRect,
    candidates: &[NavRect],
    direction: Direction,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (index, rect) in candidates.iter().enumerate() {
        let (primary, cross) = match direction {
            Direction::Up => (from.top - rect.center_y(), (rect.center_x() - from.center_x()).abs()),
            Direction::Down => (rect.center_y() - from.bottom(), (rect.center_x() - from.center_x()).abs()),
            Direction::Left => (from.left - rect.center_x(), (rect.center_y() - from.center_y()).abs()),
            Direction::Right => (rect.center_x() - from.right(), (rect.center_y() - from.center_y()).abs()),
        };
        if primary <= 0.0 {
            continue;
        }
        let score = primary + CROSS_AXIS_WEIGHT * cross;
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
}

/// The control an arrow lands on.
///
/// `origin` is `None` when focus sits on `<body>` (after a route change or first
/// load): `<body>` is never an origin even with layout geometry, because every
/// control lies inside its box and none is ever "beyond" it — the first press
/// would do nothing. With no origin, or a zero-size one, the first press lands
/// on the topmost-leftmost control.
pub fn pick_target(origin: Option<&NavRect>, candidates: &[NavRect], direction: Direction) -> Option<usize> {
    if let Some(from) = origin.filter(|from| from.width > 0.0 || from.height > 0.0) {
        return best_candidate_in_direction(from, candidates, direction);
    }
    let mut best: Option<(usize, &NavRect)> = None;
    for (index, rect) in candidates.iter().enumerate() {
        let better = best.is_none_or(|(_, current)| {
            rect.top < current.top || (rect.top == current.top && rect.left < current.left)
        });
        if better {
            best = Some((index, rect));
        }
    }
    best.map(|(index, _)| index)
}

/// Whether a focused terminal scroll box still owns ↑/↓: the browser scrolls it
/// until it clamps, and only at the edge does focus leave the pane instead of
/// dead-ending. ←/→ never belong to the box.
pub fn scroll_box_can_still_move(direction: Direction, geometry: &ScrollBoxGeometry) -> bool {
    match direction {
        Direction::Up => geometry.scroll_top > 0.0,
        Direction::Down => geometry.scroll_top < geometry.scroll_height - geometry.client_height,
        Direction::Left | Direction::Right => false,
    }
}

/// What the focused element is, as far as the arrow gate cares.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FocusedArrowOwner {
    /// An `<input>`, `<textarea>` or contenteditable: the caret owns arrows.
    pub editable: bool,
    /// Inside a menu, listbox or combobox, which rove focus themselves.
    pub in_roving_role: bool,
    /// The focused `.wterm` scroll box's geometry, when focus is on one.
    pub terminal_scroll_box: Option<ScrollBoxGeometry>,
}

/// One keydown, as far as the arrow gate cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArrowKeydown<'key> {
    /// `KeyboardEvent.key`.
    pub key: &'key str,
    /// Whether an earlier handler already claimed it.
    pub default_prevented: bool,
    /// Any of Meta / Ctrl / Alt / Shift held.
    pub modified: bool,
}

/// The direction spatial navigation may claim for this keydown, or `None` to
/// leave it alone.
///
/// Spatial navigation is the "nobody claimed this arrow" fallback: it acts only
/// in a directional modality, never on a key another handler cancelled or a
/// chord, and never where the focused element owns the arrow itself.
pub fn claimable_direction(
    directional_input_active: bool,
    keydown: &ArrowKeydown<'_>,
    focused: Option<&FocusedArrowOwner>,
) -> Option<Direction> {
    if !directional_input_active || keydown.default_prevented || keydown.modified {
        return None;
    }
    let direction = Direction::from_key(keydown.key)?;
    if let Some(owner) = focused {
        if owner.editable || owner.in_roving_role {
            return None;
        }
        if owner
            .terminal_scroll_box
            .is_some_and(|extent| scroll_box_can_still_move(direction, &extent))
        {
            return None;
        }
    }
    Some(direction)
}
