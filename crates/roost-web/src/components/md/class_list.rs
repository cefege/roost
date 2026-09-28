//! Class-attribute assembly for the md primitives: the fixed class a primitive
//! owns, its modifiers, and the caller's extra class, joined with single
//! spaces. Called by every primitive that takes a `class` prop.
//!
//! v2 built these with template strings (`apps/web/src/components/Settings/md/*.tsx`),
//! which left doubled and trailing spaces wherever a modifier was absent. The
//! class TOKENS are v2's exactly; only the whitespace between them is normalised,
//! which no selector can observe.

/// Join the non-empty parts into one class attribute value.
pub fn class_list<'part>(parts: impl IntoIterator<Item = &'part str>) -> String {
    let mut joined = String::new();
    for part in parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(part);
    }
    joined
}
