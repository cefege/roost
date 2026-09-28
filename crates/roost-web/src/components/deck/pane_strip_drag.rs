//! A tab drag inside one pane strip: which slot the dragged tab's centre
//! has crossed into, the resting offset its release springs to, the shift
//! every other tab takes, and the order a release commits. Read by
//! `pane_strip` and its pointer adapter. Pure; ports the drag math of
//! `apps/web/src/components/deck/PaneStrip.tsx`.

use super::inline_style::{InlineStyle, px};

/// One tab's measured box along the rail, in client px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabRect {
    /// Left edge.
    pub left: f64,
    /// Width.
    pub width: f64,
    /// Horizontal centre.
    pub center: f64,
}

impl TabRect {
    /// A box from its left edge and width.
    pub fn new(left: f64, width: f64) -> Self {
        Self {
            left,
            width,
            center: left + width / 2.0,
        }
    }
}

/// A live tab drag.
#[derive(Debug, Clone, PartialEq)]
pub struct TabDrag {
    /// The dragged tab.
    pub id: String,
    /// Where it started.
    pub from_idx: usize,
    /// The slot its centre is over now.
    pub to_idx: usize,
    /// Pointer travel, px.
    pub dx: f64,
    /// How far a displaced tab shifts: the dragged tab's width plus its gap.
    pub slot: f64,
    /// Every tab's box when the drag armed.
    pub rects: Vec<TabRect>,
    /// Released and springing to rest.
    pub released: bool,
}

impl TabDrag {
    /// A drag armed on the tab at `from_idx`.
    pub fn arm(id: String, from_idx: usize, dx: f64, rects: Vec<TabRect>) -> Option<Self> {
        let slot = rects.get(from_idx)?.width + 2.0;
        Some(Self {
            id,
            from_idx,
            to_idx: from_idx,
            dx,
            slot,
            rects,
            released: false,
        })
    }

    /// Follow the pointer: the dragged centre walks past neighbour centres.
    pub fn moved(&self, dx: f64) -> Self {
        let Some(origin) = self.rects.get(self.from_idx) else {
            return self.clone();
        };
        let center = origin.center + dx;
        let mut to_idx = self.from_idx;
        while to_idx + 1 < self.rects.len() && center > self.rects[to_idx + 1].center {
            to_idx += 1;
        }
        while to_idx > 0 && center < self.rects[to_idx - 1].center {
            to_idx -= 1;
        }
        Self {
            dx,
            to_idx,
            ..self.clone()
        }
    }

    /// The offset the dragged tab rests at in its new slot.
    pub fn resting_dx(&self) -> f64 {
        if self.to_idx == self.from_idx {
            return 0.0;
        }
        let (Some(from), Some(to)) = (self.rects.get(self.from_idx), self.rects.get(self.to_idx))
        else {
            return 0.0;
        };
        if self.to_idx > self.from_idx {
            to.left + to.width - from.width - from.left
        } else {
            to.left - from.left
        }
    }

    /// The tab order a release commits.
    pub fn reordered(&self, ids: &[String]) -> Vec<String> {
        let mut ordered = ids.to_vec();
        if self.from_idx < ordered.len() {
            let moved = ordered.remove(self.from_idx);
            ordered.insert(self.to_idx.min(ordered.len()), moved);
        }
        ordered
    }

    /// The tab at `index`'s transform: the dragged one follows the pointer,
    /// the ones it passed shift one slot back toward its origin.
    pub fn tab_style(&self, index: usize) -> InlineStyle {
        if index == self.from_idx {
            return InlineStyle::new()
                .with("transform", format!("translateX({})", px(self.dx)))
                .with("transition", "none")
                .with("z-index", "3");
        }
        let shift = if self.from_idx < self.to_idx && index > self.from_idx && index <= self.to_idx
        {
            -self.slot
        } else if self.from_idx > self.to_idx && index >= self.to_idx && index < self.from_idx {
            self.slot
        } else {
            0.0
        };
        let transform = if shift == 0.0 {
            "translateX(0)".to_owned()
        } else {
            format!("translateX({})", px(shift))
        };
        InlineStyle::new().with("transform", transform).with(
            "transition",
            "transform var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized)",
        )
    }
}
