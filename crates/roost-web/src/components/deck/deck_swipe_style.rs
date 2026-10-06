//! Every style the compact deck paints from a live swipe: the slot slide, the
//! phone bar riding with its card, the new-terminal peel and FAB, and the peek
//! surface behind them. Read by `terminal_deck`, `terminal_deck_chrome` and
//! `terminal_deck_swipe_overlay`. Pure; ports the presentation half of
//! `apps/web/src/lib/deckSwipe.ts`.

use roost_client_core::store::layout::PaneRect;

use super::deck_swipe::{
    NEW_BLOOM_MS, SettleTarget, Swipe, SwipeMode, SwipePhase, new_fab_progress,
};
use super::inline_style::{InlineStyle, css_number, px};
use super::terminal_deck_geometry::MOBILE_TERMINAL_STRIP_HEIGHT;

/// The current card shrinks to this at the armed point.
pub const PEEK_SCALE_MIN: f64 = 0.9;
/// …and slides left by this fraction of the width.
pub const PEEK_SHIFT_FRAC: f64 = 0.05;
/// …and rounds its corners to this, px.
pub const PEEK_RADIUS_PX: f64 = 28.0;
/// The FAB's scale at rest.
pub const NEW_FAB_MIN_SCALE: f64 = 0.5;
const FAB_PX: f64 = 56.0;
const SWIPE_DECEL: &str =
    "var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))";
const SWIPE_SLIDE_EASE: &str = "cubic-bezier(0.25, 0.46, 0.45, 0.94)";

/// The peeled card at progress `p`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeekCard {
    /// Uniform scale.
    pub scale: f64,
    /// Horizontal shift as a fraction of the width.
    pub shift_frac: f64,
    /// Corner radius, px.
    pub radius: f64,
}

/// The peel for progress `p`, clamped to `[0, 1]`.
pub fn peek_card(progress: f64) -> PeekCard {
    let clamped = progress.clamp(0.0, 1.0);
    PeekCard {
        scale: 1.0 - clamped * (1.0 - PEEK_SCALE_MIN),
        shift_frac: -clamped * PEEK_SHIFT_FRAC + 0.0,
        radius: clamped * PEEK_RADIUS_PX,
    }
}

/// The FAB's scale for progress `p`, clamped to `[0, 1]`.
pub fn new_fab_scale(progress: f64) -> f64 {
    NEW_FAB_MIN_SCALE + (1.0 - NEW_FAB_MIN_SCALE) * progress.clamp(0.0, 1.0)
}

/// The two slots' x offsets: finger-follow while tracking, a full width off
/// (or back) while settling.
pub fn swipe_offsets_px(swipe: &Swipe, width: f64) -> (f64, f64) {
    let sign = swipe.dir.sign();
    if swipe.phase == SwipePhase::Settle {
        return if swipe.settle_target == Some(SettleTarget::Commit) {
            (-sign * width, 0.0)
        } else {
            (0.0, sign * width)
        };
    }
    (swipe.offset, swipe.offset + sign * width)
}

/// The swipe layer a slot or bar composes over its placement. Every state
/// declares the same transform, origin, shadow and transition, at rest when
/// the swipe does not involve the element: Dioxus 0.7 keeps each inline
/// property a new `style` string omits, so a layer that went empty left the
/// last swipe frame on the element. The peel's corner and clip are declared
/// by every placement it composes over (`SlotStyle`, [`mobile_bar_style`]).
pub fn swipe_style_for(swipe: Option<&Swipe>, session_id: &str, width: f64) -> InlineStyle {
    let rest = swipe_layer_at_rest();
    let Some(swipe) = swipe else {
        return rest;
    };
    let is_current = session_id == swipe.current_id;
    if !is_current && swipe.neighbor_id.as_deref() != Some(session_id) {
        return rest;
    }
    let settle_ms = swipe.settle_ms.unwrap_or(200);
    let transition = if swipe.phase == SwipePhase::Settle {
        format!("transform {settle_ms}ms {SWIPE_SLIDE_EASE}")
    } else {
        "none".to_owned()
    };
    let (current, neighbor) = swipe_offsets_px(swipe, width);
    if is_current {
        return match swipe.mode {
            SwipeMode::NewTerminal => rest.merged(&new_terminal_peel(swipe, width, settle_ms)),
            SwipeMode::Workspace => rest,
            SwipeMode::Slide => rest
                .with("transform", format!("translateX({})", px(current)))
                .with("transition", transition),
        };
    }
    rest.with("transform", format!("translateX({})", px(neighbor)))
        .with("transition", transition)
}

/// A phone bar's wrapper: pinned above its terminal, riding the swipe with
/// it. It states the corner and clip the peel writes, so a peeled bar does
/// not stay rounded and clipped once the pull is gone.
pub fn mobile_bar_style(swipe: Option<&Swipe>, session_id: &str, width: f64) -> InlineStyle {
    InlineStyle::new()
        .with("position", "absolute")
        .with("left", "0")
        .with("top", "0")
        .with("width", "100%")
        .with("height", px(MOBILE_TERMINAL_STRIP_HEIGHT))
        .with("z-index", "3")
        .with("border-radius", "0")
        .with("overflow", "visible")
        .merged(&swipe_style_for(swipe, session_id, width))
}

fn swipe_layer_at_rest() -> InlineStyle {
    InlineStyle::new()
        .with("transform", "none")
        .with("transform-origin", "center center")
        .with("box-shadow", "none")
        .with("transition", "none")
}

fn new_terminal_peel(swipe: &Swipe, width: f64, settle_ms: u64) -> InlineStyle {
    let progress = match (swipe.phase, swipe.settle_target) {
        (SwipePhase::Settle, Some(SettleTarget::Commit)) => 1.0,
        (SwipePhase::Settle, _) => 0.0,
        (SwipePhase::Track, _) => new_fab_progress(swipe.offset, width),
    };
    let peek = peek_card(progress);
    let shadow = if progress > 0.0 {
        format!(
            "0 {}px {}px color-mix(in srgb, var(--md-shadow) {}%, transparent)",
            (8.0 * progress).round(),
            (30.0 * progress).round(),
            (50.0 * progress).round()
        )
    } else {
        "none".to_owned()
    };
    let transition = if swipe.phase == SwipePhase::Settle {
        format!(
            "transform {settle_ms}ms {SWIPE_DECEL}, border-radius {settle_ms}ms {SWIPE_DECEL}, box-shadow {settle_ms}ms {SWIPE_DECEL}"
        )
    } else {
        "none".to_owned()
    };
    InlineStyle::new()
        .with(
            "transform",
            format!(
                "translateX({}) scale({})",
                px(peek.shift_frac * width),
                css_number(peek.scale)
            ),
        )
        .with("transform-origin", "center center")
        .with("border-radius", px(peek.radius))
        .with("overflow", "hidden")
        .with("box-shadow", shadow)
        .with("transition", transition)
}

fn hidden() -> InlineStyle {
    InlineStyle::new().with("display", "none")
}

/// The surface the shrinking terminal reveals during a new-terminal pull.
pub fn new_peek_style(
    swipe: Option<&Swipe>,
    rect: Option<PaneRect>,
    width: f64,
    strip_height: f64,
) -> InlineStyle {
    let (Some(swipe), Some(rect)) = (swipe, rect) else {
        return hidden();
    };
    if swipe.mode != SwipeMode::NewTerminal || width <= 0.0 {
        return hidden();
    }
    let progress = new_fab_progress(swipe.offset, width);
    let settling = swipe.phase == SwipePhase::Settle;
    let committing = settling && swipe.settle_target == Some(SettleTarget::Commit);
    let opacity = if committing {
        "1".to_owned()
    } else if settling {
        "0".to_owned()
    } else {
        css_number(progress.min(1.0))
    };
    let transition = if settling {
        format!(
            "opacity {}ms {SWIPE_DECEL}",
            swipe.settle_ms.unwrap_or(NEW_BLOOM_MS)
        )
    } else {
        "none".to_owned()
    };
    InlineStyle::new()
        .with("position", "absolute")
        .with("left", "0px")
        .with("top", px(rect.y + strip_height))
        .with("width", px(rect.w))
        .with("height", px((rect.h - strip_height).max(0.0)))
        .with("z-index", "1")
        .with("opacity", opacity)
        .with("transition", transition)
}

/// The + FAB that grows under the finger and blooms into the new terminal.
pub fn new_fab_style(
    swipe: Option<&Swipe>,
    rect: Option<PaneRect>,
    width: f64,
    strip_height: f64,
) -> InlineStyle {
    let (Some(swipe), Some(rect)) = (swipe, rect) else {
        return hidden();
    };
    if swipe.mode != SwipeMode::NewTerminal || width <= 0.0 {
        return hidden();
    }
    let area_top = rect.y + strip_height;
    let area_height = (rect.h - strip_height).max(0.0);
    let settling = swipe.phase == SwipePhase::Settle;
    if settling && swipe.settle_target == Some(SettleTarget::Commit) {
        let bloom = ["left", "top", "width", "height", "border-radius"]
            .map(|property| format!("{property} {NEW_BLOOM_MS}ms {SWIPE_DECEL}"))
            .join(", ");
        return InlineStyle::new()
            .with("position", "absolute")
            .with("left", "0px")
            .with("top", px(area_top))
            .with("width", px(rect.w))
            .with("height", px(area_height))
            .with("border-radius", "0px")
            .with("transform", "scale(1)")
            .with("opacity", "1")
            .with("z-index", "6")
            .with("transition", bloom);
    }
    let progress = new_fab_progress(swipe.offset, width);
    let settle_ms = swipe.settle_ms.unwrap_or(200);
    let scale = if settling {
        NEW_FAB_MIN_SCALE
    } else {
        new_fab_scale(progress)
    };
    let opacity = if settling {
        0.0
    } else {
        (progress * 1.4).min(1.0)
    };
    let transition = if settling {
        format!("transform {settle_ms}ms {SWIPE_DECEL}, opacity {settle_ms}ms {SWIPE_DECEL}")
    } else {
        "none".to_owned()
    };
    InlineStyle::new()
        .with("position", "absolute")
        .with("left", px(rect.w - 20.0 - FAB_PX))
        .with("top", px(area_top + area_height / 2.0 - FAB_PX / 2.0))
        .with("width", px(FAB_PX))
        .with("height", px(FAB_PX))
        .with("border-radius", "50%")
        .with("transform", format!("scale({})", css_number(scale)))
        .with("transform-origin", "center center")
        .with("opacity", css_number(opacity))
        .with("z-index", "6")
        .with("transition", transition)
}
