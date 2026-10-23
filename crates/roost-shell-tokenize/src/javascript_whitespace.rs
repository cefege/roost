//! ECMAScript whitespace boundaries shared by shell, settings and text consumers.
//! The code-unit form also accepts UTF-16 scanner input without decoding it.
//! Unlike Rust whitespace, this includes BOM and excludes NEL.

/// JavaScript WhiteSpace and LineTerminator characters.
#[inline]
pub const fn is_javascript_whitespace(character: char) -> bool {
    is_javascript_whitespace_code_unit(character as u32)
}

/// The same predicate for a JavaScript UTF-16 code unit or Unicode code point.
#[inline]
pub const fn is_javascript_whitespace_code_unit(unit: u32) -> bool {
    matches!(
        unit,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}
