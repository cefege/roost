//! The skeleton styles only an explicit width; the default full-width bar sets
//! no inline style. Exercises `roost_web::components::ui::skeleton`.

use roost_web::components::ui::skeleton::skeleton_style;

#[test]
fn a_skeleton_only_styles_an_explicit_width() {
    assert_eq!(skeleton_style(None), None);
    assert_eq!(
        skeleton_style(Some("30%")).as_deref(),
        Some("inline-size: 30%;")
    );
}
