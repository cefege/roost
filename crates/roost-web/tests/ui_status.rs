//! Status dots declare colour and border longhands explicitly so Dioxus style
//! patching cannot erase a token held in a shorthand. Browser specimens exercise
//! filled/hollow transitions through the real component.

use roost_web::components::ui::status::status_dot_style;

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
                matches!(
                    property,
                    "background-color"
                        | "border-top-color"
                        | "border-right-color"
                        | "border-bottom-color"
                        | "border-left-color"
                ),
                "{property} is a shorthand carrying a token in {style}"
            );
        }
    }
}
