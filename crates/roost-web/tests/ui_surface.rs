//! The surface's class list is built from theme tokens and clamped onto their
//! scales, so an out-of-range step still paints a real token. Exercises
//! `roost_web::components::ui::surface`.

use roost_web::components::ui::surface::{surface_classes, SurfaceRadius};

#[test]
fn a_surface_is_token_driven_and_clamped_onto_its_scales() {
    let plain = surface_classes(1, 0, SurfaceRadius::Md, None, false, None);
    assert!(plain.contains("bg-[var(--surface-1)]"), "{plain}");
    assert!(plain.contains("[box-shadow:var(--md-elev-0)]"), "{plain}");
    assert!(plain.contains("[border-radius:var(--md-shape-md)]"), "{plain}");
    assert!(!plain.contains("border-[var(--md-outline-variant)]"), "{plain}");

    let full = surface_classes(9, 9, SurfaceRadius::None, Some(20), true, Some("block"));
    assert!(full.contains("bg-[var(--surface-3)]"), "{full}");
    assert!(full.contains("[box-shadow:var(--md-elev-5)]"), "{full}");
    assert!(full.contains("rounded-none"), "{full}");
    assert!(full.contains("[padding:var(--md-space-9)]"), "{full}");
    assert!(full.contains("border border-[var(--md-outline-variant)]"), "{full}");
    assert!(full.contains("block"), "{full}");
}

#[test]
fn a_padding_below_the_scale_clamps_up_to_its_first_step() {
    let padded = surface_classes(0, 0, SurfaceRadius::Xl, Some(0), false, None);
    assert!(padded.contains("[padding:var(--md-space-1)]"), "{padded}");
}
