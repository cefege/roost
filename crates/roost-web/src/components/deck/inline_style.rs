//! An ordered inline-style declaration list: what the deck's geometry and
//! swipe mappings return and what a deck element's `style` attribute renders.
//! Read by `terminal_deck_geometry`, `deck_swipe_style` and the deck
//! components. Pure; stands in for the `Record<string, string>` style maps of
//! `apps/web/src/lib/deckSwipe.ts` and `components/deck/terminal-deck-geometry.ts`.

/// CSS declarations in insertion order; a later value for a property
/// replaces the earlier one in place, the way an object spread does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineStyle {
    declarations: Vec<(&'static str, String)>,
}

impl InlineStyle {
    /// No declarations.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one declaration.
    pub fn with(mut self, property: &'static str, value: impl Into<String>) -> Self {
        self.set(property, value);
        self
    }

    /// Add or replace one declaration in place.
    pub fn set(&mut self, property: &'static str, value: impl Into<String>) {
        let value = value.into();
        match self.declarations.iter_mut().find(|(name, _)| *name == property) {
            Some(existing) => existing.1 = value,
            None => self.declarations.push((property, value)),
        }
    }

    /// Spread `other` over this list.
    pub fn merged(mut self, other: &InlineStyle) -> Self {
        for (property, value) in &other.declarations {
            self.set(property, value.clone());
        }
        self
    }

    /// One property's value.
    pub fn get(&self, property: &str) -> Option<&str> {
        self.declarations
            .iter()
            .find(|(name, _)| *name == property)
            .map(|(_, value)| value.as_str())
    }

    /// Whether nothing is declared.
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// The `style` attribute text.
    pub fn css(&self) -> String {
        self.declarations
            .iter()
            .map(|(property, value)| format!("{property}: {value};"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A number as JavaScript's template literal prints it: shortest round-trip,
/// no trailing `.0`, and a negative zero as `0`.
pub fn css_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    format!("{value}")
}

/// `<n>px`.
pub fn px(value: f64) -> String {
    format!("{}px", css_number(value))
}

/// The leading number of a CSS value, read as `Number.parseFloat` does
/// (`"35px"` is 35); `None` when it does not start with one.
pub fn parse_leading_px(value: &str) -> Option<f64> {
    let trimmed = value.trim_start();
    let end = trimmed
        .char_indices()
        .find(|(index, character)| {
            !(character.is_ascii_digit() || *character == '.' || (*index == 0 && matches!(character, '-' | '+')))
        })
        .map_or(trimmed.len(), |(index, _)| index);
    trimmed[..end].parse::<f64>().ok().filter(|number| number.is_finite())
}
