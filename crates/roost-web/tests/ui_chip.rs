//! The chip's selection state: only an actual selection writes `aria-pressed`,
//! so an unselected or unstated chip carries no attribute at all. Exercises
//! `roost_web::components::ui::chip`.

use roost_web::components::ui::chip::chip_selected_attribute;

#[test]
fn a_chip_marks_only_an_actual_selection() {
    assert_eq!(chip_selected_attribute(Some(true)), Some("true"));
    assert_eq!(chip_selected_attribute(Some(false)), None);
    assert_eq!(chip_selected_attribute(None), None);
}
