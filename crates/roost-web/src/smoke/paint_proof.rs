//! The geometry half of the paint proofs: rectangle intersection, clipping and
//! two-frame stability, the marker's text-node range, the cursor's grid
//! alignment, and the proof records the oracle receives. Native; the DOM reads
//! live in `smoke::dom`, the loops in `smoke::backdoor`. Ports the geometry of
//! `apps/web/src/smoke/smokeHarness.ts:85-313` and the proof shapes of
//! `apps/web/src/renderer/terminalDiagSnapshot.ts:25-58`.

use serde::Serialize;

/// How far two reads of one rectangle may drift and still be one paint.
const STABLE_TOLERANCE_PX: f64 = 0.75;

/// A `DOMRect`, copied (`TerminalRectSnapshot`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RectSnapshot {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub width: f64,
    pub height: f64,
}

impl RectSnapshot {
    /// A rectangle from its origin and size.
    pub fn from_origin(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
            width,
            height,
        }
    }

    /// Whether both have area and overlap.
    pub fn intersects(&self, other: &Self) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && other.width > 0.0
            && other.height > 0.0
            && self.right > other.left
            && self.left < other.right
            && self.bottom > other.top
            && self.top < other.bottom
    }

    /// The part of `self` inside `clip`, or `None` when nothing is.
    pub fn clipped_to(&self, clip: &Self) -> Option<Self> {
        let left = self.left.max(clip.left);
        let top = self.top.max(clip.top);
        let right = self.right.min(clip.right);
        let bottom = self.bottom.min(clip.bottom);
        let (width, height) = (right - left, bottom - top);
        (width > 0.0 && height > 0.0).then_some(Self {
            left,
            top,
            right,
            bottom,
            width,
            height,
        })
    }

    /// Whether two reads agree within the paint tolerance on every edge.
    pub fn stable_with(&self, other: &Self) -> bool {
        [
            (self.left, other.left),
            (self.top, other.top),
            (self.right, other.right),
            (self.bottom, other.bottom),
            (self.width, other.width),
            (self.height, other.height),
        ]
        .iter()
        .all(|(left, right)| (left - right).abs() <= STABLE_TOLERANCE_PX)
    }
}

/// Where a marker starts and ends inside a row's non-empty text nodes, in the
/// UTF-16 offsets `Range.setStart`/`setEnd` take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextNodeRange {
    pub start_node: usize,
    pub start_offset: u32,
    pub end_node: usize,
    pub end_offset: u32,
}

/// Locate `marker` across the concatenated text of `nodes` (empty nodes are
/// expected to be omitted by the caller, as v2's walker omits them).
pub fn marker_text_range(nodes: &[String], marker: &str) -> Option<TextNodeRange> {
    let joined: Vec<u16> = nodes.iter().flat_map(|node| node.encode_utf16()).collect();
    let needle: Vec<u16> = marker.encode_utf16().collect();
    if needle.is_empty() || needle.len() > joined.len() {
        return None;
    }
    let marker_start = joined
        .windows(needle.len())
        .position(|window| window == needle.as_slice())?;
    let marker_end = marker_start + needle.len();
    let mut offset = 0;
    let mut start = None;
    for (index, node) in nodes.iter().enumerate() {
        let next = offset + node.encode_utf16().count();
        if start.is_none() && marker_start >= offset && marker_start < next {
            start = Some((index, (marker_start - offset) as u32));
        }
        if marker_end > offset && marker_end <= next {
            let (start_node, start_offset) = start?;
            return Some(TextNodeRange {
                start_node,
                start_offset,
                end_node: index,
                end_offset: (marker_end - offset) as u32,
            });
        }
        offset = next;
    }
    None
}

/// Whether computed style values hide an element (`hasVisibleComputedStyle`'s
/// per-element test). `opacity_counts` is false for the cursor's own blink.
pub fn style_hides(
    display: &str,
    visibility: &str,
    opacity: &str,
    content_visibility: &str,
    opacity_counts: bool,
) -> bool {
    display == "none"
        || visibility == "hidden"
        || visibility == "collapse"
        || content_visibility == "hidden"
        || (opacity_counts && opacity.trim().parse::<f64>().ok() == Some(0.0))
}

/// Whether a computed `background-color` paints nothing: `transparent`, or an
/// `rgba(…, 0)` / `… / 0)` colour.
pub fn background_is_transparent(background: &str) -> bool {
    let compact: String = background
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if compact == "transparent" {
        return true;
    }
    let Some(body) = compact.strip_suffix(')') else {
        return false;
    };
    let zero_alpha = |tail: &str| {
        let fraction = tail.strip_prefix('0').map(|rest| rest.strip_prefix('.'));
        match fraction {
            Some(None) => tail == "0",
            Some(Some(zeros)) => !zeros.is_empty() && zeros.chars().all(|digit| digit == '0'),
            None => false,
        }
    };
    let rgba_zero = compact.starts_with("rgba(")
        && !body[5..].contains(')')
        && body.rsplit_once(',').is_some_and(|(_, alpha)| zero_alpha(alpha));
    let slash_zero = body.rsplit_once('/').is_some_and(|(_, alpha)| zero_alpha(alpha));
    rgba_zero || slash_zero
}

/// `Number(element.dataset.<name>)` kept only as a non-negative integer: a
/// missing attribute is `NaN` (no coordinate), an empty one is `0`.
pub fn dataset_coordinate(value: Option<&str>) -> Option<u32> {
    let text = value?.trim();
    if text.is_empty() {
        return Some(0);
    }
    let number: f64 = text.parse().ok()?;
    (number.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&number)).then_some(number as u32)
}

/// Whether a cursor box sits on its row and in its column, within 8% of a
/// cell (never less than 0.75px).
pub fn cursor_aligned(cursor: &RectSnapshot, row: &RectSnapshot, column: u32) -> bool {
    if row.width <= 0.0 || row.height <= 0.0 {
        return false;
    }
    let row_tolerance = STABLE_TOLERANCE_PX.max(cursor.height * 0.08);
    if (cursor.top - row.top).abs() > row_tolerance || cursor.bottom > row.bottom + row_tolerance {
        return false;
    }
    let column_tolerance = STABLE_TOLERANCE_PX.max(cursor.width * 0.08);
    let expected_left = row.left + f64::from(column) * cursor.width;
    (cursor.left - expected_left).abs() <= column_tolerance
}

/// `PaintedMarkerProof` (`MarkerPresentationProof`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerProof {
    #[serde(rename = "proof_kind")]
    pub proof_kind: &'static str,
    pub session_id: String,
    pub marker: String,
    pub monotonic_ms: f64,
    pub epoch_ms: f64,
    pub row_text: String,
    pub marker_rect: RectSnapshot,
    pub terminal_rect: RectSnapshot,
    pub visual_viewport_rect: RectSnapshot,
    pub frames: u8,
}

/// `PaintedCursorProof` (`CursorPresentationProof`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorProof {
    #[serde(rename = "proof_kind")]
    pub proof_kind: &'static str,
    pub session_id: String,
    pub row: u32,
    pub column: u32,
    pub monotonic_ms: f64,
    pub epoch_ms: f64,
    pub rect: RectSnapshot,
    pub terminal_clip: RectSnapshot,
    pub visual_viewport: RectSnapshot,
    pub frames: u8,
}
