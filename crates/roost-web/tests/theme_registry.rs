//! Compatibility of persisted palette identifiers and readable theme endpoints.
//! Selection must not drop an existing base/accent combination during migration.
//! Tests exercise the public catalog, not generated stylesheet text.

use libero::theme::HexColor;
use std::collections::BTreeSet;

use roost_web::theme::catalog::{accent_themes, base_themes, theme_set_for_choice};
use roost_web::theme::choice::{ThemeChoice, ThemeMode};

#[test]
fn persisted_palette_ids_retain_their_picker_order() {
    assert_eq!(
        base_themes()
            .iter()
            .map(|theme| theme.id)
            .collect::<Vec<_>>(),
        [
            "neutral", "stone", "zinc", "mauve", "olive", "mist", "taupe"
        ]
    );
    assert_eq!(
        accent_themes().map(|theme| theme.id).collect::<Vec<_>>(),
        [
            "default", "amber", "blue", "cyan", "emerald", "fuchsia", "green", "indigo", "lime",
            "orange", "pink", "purple", "red", "rose", "sky", "teal", "violet", "yellow"
        ]
    );
}

#[test]
fn palette_ids_are_unambiguous_within_each_picker() {
    let bases: BTreeSet<&str> = base_themes().iter().map(|theme| theme.id).collect();
    assert_eq!(bases.len(), base_themes().len());
    let accents: BTreeSet<&str> = accent_themes().map(|theme| theme.id).collect();
    assert_eq!(accents.len(), accent_themes().count());
}

#[test]
fn every_saved_palette_combination_has_readable_light_and_dark_page_text() {
    for base in base_themes() {
        for accent in accent_themes() {
            let choice = ThemeChoice {
                base: base.id.to_owned(),
                accent: accent.id.to_owned(),
                mode: ThemeMode::Dark,
            };
            let pair = theme_set_for_choice(&choice);
            let light = pair.light_theme();
            let dark = pair
                .dark_theme()
                .expect("every saved palette supports dark mode");
            assert!(
                contrast_ratio(light.ink, light.surface) >= 4.5,
                "{} / {} light",
                base.id,
                accent.id
            );
            assert!(
                contrast_ratio(dark.ink, dark.surface) >= 4.5,
                "{} / {} dark",
                base.id,
                accent.id
            );
            assert_ne!(
                light.surface, dark.surface,
                "{} / {} must actually change scheme",
                base.id, accent.id
            );
        }
    }
}

fn contrast_ratio(foreground: HexColor, background: HexColor) -> f64 {
    let luminance = |color: HexColor| {
        [color.r(), color.g(), color.b()]
            .into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(channel, weight)| {
                let normalized = f64::from(channel) / 255.0;
                weight
                    * if normalized <= 0.04045 {
                        normalized / 12.92
                    } else {
                        ((normalized + 0.055) / 1.055).powf(2.4)
                    }
            })
            .sum::<f64>()
    };
    let foreground = luminance(foreground);
    let background = luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}
