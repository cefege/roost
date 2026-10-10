//! The md primitives' markup rules: the classes each emits for its variants,
//! the inline styles built from tokens, and the attributes that are written only
//! in some states. Exercises `roost_web::components::md`, the port of
//! `apps/web/src/components/Settings/md/*.tsx`; the stylesheets
//! (`controls.css`, `tokens.css`, `icon.css`) select on exactly these tokens.

use dioxus::prelude::{Attribute, Modifiers};
use roost_web::components::md::button::{button_class, names_button_type};
use roost_web::components::md::card::card_class;
use roost_web::components::md::chip::chip_selected_attribute;
use roost_web::components::md::class_list::class_list;
use roost_web::components::md::form_field::described_by;
use roost_web::components::md::icon::icon_class;
use roost_web::components::md::icon_button::icon_button_aria;
use roost_web::components::md::list::list_class;
use roost_web::components::md::list_row::{is_in_app_navigation_click, list_row_class};
use roost_web::components::md::progress_bar::progress_fill_width;
use roost_web::components::md::sheet::sheet_class;
use roost_web::components::md::skeleton::skeleton_style;
use roost_web::components::md::status_dot::{status_dot_style, status_dot_token};
use roost_web::components::md::surface::surface_style;
use roost_web::components::md::text_field::text_field_invalid;
use roost_web::components::md::{
    ButtonSize, ButtonVariant, CardVariant, IconSize, ListLayout, SheetSide, SurfaceRadius,
};

#[test]
fn class_lists_drop_absent_modifiers_without_stray_spaces() {
    assert_eq!(class_list(["md-icon", "", "  ", "extra "]), "md-icon extra");
    assert_eq!(class_list([""]), "");
}

#[test]
fn a_button_carries_its_variant_and_size_modifiers() {
    assert_eq!(
        button_class(ButtonVariant::Default, ButtonSize::Default, None),
        "roost-button roost-button--default roost-button--default"
    );
    assert_eq!(
        button_class(
            ButtonVariant::Destructive,
            ButtonSize::IconSm,
            Some("df-tab-close")
        ),
        "roost-button roost-button--destructive roost-button--icon-sm df-tab-close"
    );
    assert_eq!(
        button_class(ButtonVariant::Link, ButtonSize::Xs, None),
        "roost-button roost-button--link roost-button--xs"
    );
}

#[test]
fn a_caller_type_override_suppresses_the_button_default() {
    // A second `type` attribute beside a caller's `type="submit"` would leave
    // the browser to pick one; the default must step aside.
    let submit = vec![Attribute::new("type", "submit", None, false)];
    let labelled = vec![Attribute::new("aria-label", "Close", None, false)];
    assert!(names_button_type(&submit));
    assert!(!names_button_type(&labelled));
    assert!(!names_button_type(&[]));
}

#[test]
fn an_icon_button_names_itself_and_claims_a_popup_only_when_it_has_one() {
    assert_eq!(
        icon_button_aria("More", None, None, None),
        vec![("aria-label", "More".to_string())]
    );
    assert_eq!(
        icon_button_aria(
            "Open context menu",
            Some("menu"),
            Some("design-context-menu"),
            Some(false)
        ),
        vec![
            ("aria-label", "Open context menu".to_string()),
            ("aria-haspopup", "menu".to_string()),
            ("aria-controls", "design-context-menu".to_string()),
            ("aria-expanded", "false".to_string()),
        ]
    );
}

#[test]
fn icon_modifiers_follow_fill_and_size() {
    assert_eq!(icon_class(false, IconSize::Md, None), "md-icon");
    assert_eq!(
        icon_class(true, IconSize::Sm, None),
        "md-icon md-icon--filled md-icon--sm"
    );
    assert_eq!(
        icon_class(false, IconSize::Lg, Some("md-empty-state__icon")),
        "md-icon md-icon--lg md-empty-state__icon"
    );
}

#[test]
fn card_list_row_and_sheet_classes_match_the_stylesheet() {
    assert_eq!(card_class(CardVariant::Filled, None), "md-card");
    assert_eq!(
        card_class(CardVariant::Elevated, None),
        "md-card md-card--elevated"
    );
    assert_eq!(
        card_class(CardVariant::Outlined, Some("x")),
        "md-card md-card--outlined x"
    );
    assert_eq!(list_class(false, ListLayout::Stack, None), "md-list");
    assert_eq!(
        list_class(true, ListLayout::Grid, None),
        "md-list md-list--container md-list--grid"
    );
    assert_eq!(
        list_row_class(true, Some("settings-rail__item")),
        "md-list-row md-list-row--dense settings-rail__item"
    );
    assert_eq!(sheet_class(SheetSide::Right, None), "roost-sheet--right");
    assert_eq!(
        sheet_class(SheetSide::Bottom, Some("roost-dialog--browse")),
        "roost-sheet--bottom roost-dialog--browse"
    );
    assert_eq!(sheet_class(SheetSide::Center, None), "roost-sheet--center");
}

#[test]
fn a_destination_row_hands_only_plain_primary_clicks_to_the_router() {
    assert!(is_in_app_navigation_click(true, Modifiers::empty()));
    assert!(!is_in_app_navigation_click(true, Modifiers::CONTROL));
    assert!(!is_in_app_navigation_click(true, Modifiers::META));
    assert!(!is_in_app_navigation_click(true, Modifiers::SHIFT));
    assert!(!is_in_app_navigation_click(false, Modifiers::empty()));
}

#[test]
fn every_status_maps_to_its_canonical_token() {
    for (status, token) in [
        ("ok", "--status-ok"),
        ("done", "--status-ok"),
        ("running", "--md-primary"),
        ("idle", "--text-lo"),
        ("offline", "--text-lo"),
        ("warn", "--status-warn"),
        ("error", "--status-err"),
        ("info", "--status-info"),
        ("something-new", "--text-lo"),
    ] {
        assert_eq!(status_dot_token(status), token, "{status}");
    }
}

#[test]
fn a_status_dot_is_filled_or_ringed_at_its_size() {
    let solid = status_dot_style("error", 8, false);
    assert!(solid.contains("width: 8px; height: 8px;"), "{solid}");
    assert!(
        solid.contains("background-color: var(--status-err);"),
        "{solid}"
    );
    assert!(!solid.contains("border-color"), "{solid}");
    let hollow = status_dot_style("ok", 12, true);
    assert!(hollow.contains("width: 12px; height: 12px;"), "{hollow}");
    assert!(
        hollow.contains("background-color: transparent;"),
        "{hollow}"
    );
    assert!(
        hollow.contains("border-color: var(--status-ok);"),
        "{hollow}"
    );
}

/// Dioxus re-applies, after every `style` update, each property the new value
/// reports as empty, and a shorthand holding `var()` reports its longhands
/// empty: the update erases the colour. A dot's token must ride a longhand.
#[test]
fn a_status_dot_never_puts_its_token_in_a_shorthand() {
    for (status, hollow) in [
        ("ok", false),
        ("running", false),
        ("idle", true),
        ("error", true),
    ] {
        let style = status_dot_style(status, 8, hollow);
        for declaration in style
            .split(';')
            .map(str::trim)
            .filter(|part| part.contains("var("))
        {
            let property = declaration.split(':').next().unwrap_or_default().trim();
            assert!(
                property.contains('-'),
                "{property} is a shorthand carrying a token in {style}"
            );
        }
    }
}

#[test]
fn a_surface_is_token_driven_and_clamped_onto_its_scales() {
    assert_eq!(
        surface_style(1, 0, SurfaceRadius::Md, None, false, None),
        "background: var(--surface-1); box-shadow: var(--md-elev-0); border-radius: var(--md-shape-md);"
    );
    let full = surface_style(
        9,
        9,
        SurfaceRadius::None,
        Some(20),
        true,
        Some("display: block;"),
    );
    assert_eq!(
        full,
        "background: var(--surface-3); box-shadow: var(--md-elev-5); border-radius: 0; \
         padding: var(--md-space-9); border: 1px solid var(--md-outline-variant); display: block;"
    );
    assert!(
        surface_style(0, 0, SurfaceRadius::Xl, Some(0), false, None)
            .contains("padding: var(--md-space-1);")
    );
}

#[test]
fn caller_style_comes_last_so_it_wins_layout_ties() {
    let style = surface_style(
        2,
        1,
        SurfaceRadius::Lg,
        Some(4),
        false,
        Some("background: var(--surface-0);"),
    );
    assert!(style.ends_with("background: var(--surface-0);"), "{style}");
}

#[test]
fn the_progress_fill_is_clamped_to_its_track() {
    assert_eq!(progress_fill_width(0.42), "width: 42.0%;");
    assert_eq!(progress_fill_width(1.7), "width: 100.0%;");
    assert_eq!(progress_fill_width(-0.2), "width: 0.0%;");
    assert_eq!(progress_fill_width(f64::NAN), "width: 0.0%;");
}

#[test]
fn a_skeleton_only_styles_an_explicit_width() {
    assert_eq!(skeleton_style(None), None);
    assert_eq!(
        skeleton_style(Some("30%")).as_deref(),
        Some("inline-size: 30%;")
    );
}

#[test]
fn a_chip_marks_only_an_actual_selection() {
    assert_eq!(chip_selected_attribute(Some(true)), Some("true"));
    assert_eq!(chip_selected_attribute(Some(false)), None);
    assert_eq!(chip_selected_attribute(None), None);
}

#[test]
fn a_description_reads_the_caller_then_the_help_then_the_error() {
    assert_eq!(described_by(None, None, None), None);
    assert_eq!(described_by(Some(""), None, None), None);
    assert_eq!(
        described_by(Some("hint"), Some("f-description"), Some("f-error")).as_deref(),
        Some("hint f-description f-error")
    );
    assert_eq!(
        described_by(None, None, Some("f-error")).as_deref(),
        Some("f-error")
    );
}

#[test]
fn a_text_field_is_invalid_when_told_or_when_it_has_an_error() {
    assert!(!text_field_invalid(false, false));
    assert!(text_field_invalid(true, false));
    assert!(text_field_invalid(false, true));
}
