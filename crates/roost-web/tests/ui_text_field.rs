//! A text field reads as invalid when the caller says so or when it shows an error.

use roost_web::components::ui::text_field::text_field_invalid;

#[test]
fn a_text_field_is_invalid_when_told_or_when_it_has_an_error() {
    assert!(!text_field_invalid(false, false));
    assert!(text_field_invalid(true, false));
    assert!(text_field_invalid(false, true));
}
