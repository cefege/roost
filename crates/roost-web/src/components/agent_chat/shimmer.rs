//! OMP's classic shimmer for the working row: a cosine band of brightness that
//! travels across the label at a fixed cell speed and is read as three tiers.
//! The tier maths is pure so the sweep can be pinned in unit tests; the
//! `ShimmerLabel` component draws one span per code point, as OMP does.

use dioxus::prelude::*;

/// Band travel in label cells per second; fixed so the sweep speed does not
/// depend on the label's length.
const SPEED_CELLS_PER_SECOND: f64 = 30.0;
/// Empty cells before and after the label, so the band enters and leaves it.
const CLASSIC_PADDING: f64 = 10.0;
const CLASSIC_BAND_HALF_WIDTH: f64 = 6.0;
const TIER_HIGH: f64 = 0.65;
const TIER_MID: f64 = 0.22;

/// Brightness tier of one character: accent-bold, muted, or dim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShimmerTier {
    Low,
    Mid,
    High,
}

impl ShimmerTier {
    pub(super) const fn class_name(self) -> &'static str {
        match self {
            Self::Low => "agent-chat__working-char agent-chat__working-char--low",
            Self::Mid => "agent-chat__working-char agent-chat__working-char--mid",
            Self::High => "agent-chat__working-char agent-chat__working-char--high",
        }
    }
}

/// Cosine bump centred on `position`; zero outside the band.
pub(super) fn classic_intensity(index: usize, position: f64) -> f64 {
    let distance = (index as f64 + CLASSIC_PADDING - position).abs();
    if distance >= CLASSIC_BAND_HALF_WIDTH {
        return 0.0;
    }
    0.5 * (1.0 + (std::f64::consts::PI * distance / CLASSIC_BAND_HALF_WIDTH).cos())
}

pub(super) fn tier_for(intensity: f64) -> ShimmerTier {
    if intensity >= TIER_HIGH {
        ShimmerTier::High
    } else if intensity >= TIER_MID {
        ShimmerTier::Mid
    } else {
        ShimmerTier::Low
    }
}

/// Band centre in cells for a label of `code_points` characters, wrapping over
/// the label plus both paddings.
pub(super) fn sweep_position(elapsed_seconds: f64, code_points: usize) -> f64 {
    let period = code_points as f64 + 2.0 * CLASSIC_PADDING;
    (elapsed_seconds * SPEED_CELLS_PER_SECOND).rem_euclid(period)
}

/// Each code point of `text` with its tier at `elapsed_seconds`.
pub(super) fn shimmer_tiers(text: &str, elapsed_seconds: f64) -> Vec<(char, ShimmerTier)> {
    let position = sweep_position(elapsed_seconds, text.chars().count());
    text.chars()
        .enumerate()
        .map(|(index, ch)| (ch, tier_for(classic_intensity(index, position))))
        .collect()
}

/// The shimmering label. The full text stays in `aria-label`; the per-character
/// spans are hidden from assistive technology.
#[component]
pub(super) fn ShimmerLabel(text: String, elapsed_seconds: f64) -> Element {
    let tiers = shimmer_tiers(&text, elapsed_seconds);
    rsx! {
        span { class: "agent-chat__working-label",
            for entry in tiers {
                span { class: "{entry.1.class_name()}", "aria-hidden": "true", "{entry.0}" }
            }
            span { class: "agent-chat__working-sr", "{text}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_centre_is_fully_bright_and_outside_the_band_is_dark() {
        assert!((classic_intensity(0, 10.0) - 1.0).abs() < f64::EPSILON);
        assert_eq!(classic_intensity(0, 10.0 + CLASSIC_BAND_HALF_WIDTH), 0.0);
    }

    #[test]
    fn tiers_split_at_the_omp_thresholds() {
        assert_eq!(tier_for(0.65), ShimmerTier::High);
        assert_eq!(tier_for(0.64), ShimmerTier::Mid);
        assert_eq!(tier_for(0.22), ShimmerTier::Mid);
        assert_eq!(tier_for(0.21), ShimmerTier::Low);
    }

    #[test]
    fn sweep_wraps_over_label_plus_padding() {
        let period = 5.0 + 2.0 * CLASSIC_PADDING;
        let wrapped = sweep_position(period / SPEED_CELLS_PER_SECOND, 5);
        assert!(wrapped.abs() < 1e-9);
    }

    #[test]
    fn shimmer_tiers_brighten_the_character_under_the_band() {
        // Position 10 puts the band centre on index 0; index 6 sits on the band edge.
        let elapsed = 10.0 / SPEED_CELLS_PER_SECOND;
        let tiers = shimmer_tiers("abcdefg", elapsed);
        assert_eq!(tiers[0], ('a', ShimmerTier::High));
        assert_eq!(tiers[6], ('g', ShimmerTier::Low));
    }
}
