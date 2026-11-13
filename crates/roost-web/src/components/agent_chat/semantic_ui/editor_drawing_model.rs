//! UTF-16 decoration ranges and caret splits for the native editor drawing.
//! The HTML control remains the text owner; this model never changes its value.
//! Styling delegates to semantic spans and the existing code tokenizer.

use crate::syntax_lite::{self, TokenKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decoration {
    from: usize,
    to: usize,
    s: String,
    #[serde(default)]
    fx: Option<String>,
}

#[derive(Debug)]
pub(super) struct DrawingRun {
    pub text: String,
    pub syntax: TokenKind,
    pub decorations: Vec<Value>,
}

pub(super) struct Drawing {
    pub before: Vec<DrawingRun>,
    pub after: Vec<DrawingRun>,
}

pub(super) fn prepare(
    text: &str,
    cursor: usize,
    decor: Option<&Value>,
    language: Option<&str>,
) -> Result<Drawing, String> {
    let decorations: Vec<Decoration> = decor
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| "OMP editor decorations have an unsupported shape.".to_owned())?
        .unwrap_or_default();
    let caret = byte_at_utf16(text, cursor).unwrap_or_else(|| floor_byte_at_utf16(text, cursor));
    let mut boundaries = BTreeSet::from([0, caret, text.len()]);
    let mut ranges = Vec::with_capacity(decorations.len());
    for decoration in decorations {
        let from = byte_at_utf16(text, decoration.from);
        let to = byte_at_utf16(text, decoration.to);
        let (Some(from), Some(to)) = (from, to) else {
            return Err("OMP editor decoration is not a valid UTF-16 range.".into());
        };
        if from > to {
            return Err("OMP editor decoration has a reversed range.".into());
        }
        if !super::super::super::content::editor_style_supported(&decoration.s)
            || !matches!(
                decoration.fx.as_deref(),
                None | Some("none" | "pulse" | "shimmer")
            )
        {
            return Err("OMP editor decoration requests an unsupported style or effect.".into());
        }
        boundaries.extend([from, to]);
        ranges.push((from, to, decoration));
    }
    let mut syntax = Vec::new();
    if language.is_some_and(|language| !language.is_empty()) {
        let mut offset = 0;
        for (line_index, tokens) in syntax_lite::tokenize_lines(text.split('\n'))
            .into_iter()
            .enumerate()
        {
            if line_index > 0 {
                offset += 1;
            }
            for token in tokens {
                let end = offset + token.text.len();
                boundaries.extend([offset, end]);
                syntax.push((offset, end, token.kind));
                offset = end;
            }
        }
    }
    let mut drawing = Drawing {
        before: Vec::new(),
        after: Vec::new(),
    };
    let boundaries: Vec<_> = boundaries.into_iter().collect();
    for pair in boundaries.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from == to {
            continue;
        }
        let text = text[from..to].to_owned();
        let decorations = ranges
            .iter()
            .filter(|(start, end, _)| *start <= from && *end >= to)
            .map(|(_, _, decoration)| json!({"s":decoration.s,"fx":decoration.fx}))
            .collect();
        let syntax = syntax
            .iter()
            .find(|(start, end, _)| *start <= from && *end >= to)
            .map_or(TokenKind::Plain, |(_, _, kind)| *kind);
        let run = DrawingRun {
            text,
            syntax,
            decorations,
        };
        if to <= caret {
            drawing.before.push(run);
        } else {
            drawing.after.push(run);
        }
    }
    Ok(drawing)
}

fn byte_at_utf16(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

fn floor_byte_at_utf16(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        units += character.len_utf16();
        if units > offset {
            return byte;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::prepare;
    use serde_json::json;

    #[test]
    fn multiline_ghost_position_splits_at_utf16_caret_not_empty_text() {
        let drawing = prepare("a😀\nrest", 4, None, None).expect("drawing");
        assert_eq!(
            drawing
                .before
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "a😀\n"
        );
        assert_eq!(
            drawing
                .after
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "rest"
        );
    }

    #[test]
    fn overlapping_diagnostics_keep_both_styles_across_the_caret() {
        let drawing = prepare(
            "a😀bc",
            3,
            Some(&json!([
                {"from":1,"to":4,"s":"accent","fx":"shimmer"},
                {"from":3,"to":5,"s":"typo"}
            ])),
            None,
        )
        .expect("drawing");
        assert_eq!(drawing.after[0].text, "b");
        assert_eq!(drawing.after[0].decorations.len(), 2);
        assert_eq!(drawing.before[1].text, "😀");
        assert_eq!(drawing.before[1].decorations[0]["fx"], "shimmer");
    }

    #[test]
    fn unsupported_fields_and_surrogate_splits_fail_visibly() {
        for decor in [
            json!([{"from":0,"to":1,"s":"typo","tooltip":"lost"}]),
            json!([{"from":0,"to":1,"s":"unknown"}]),
            json!([{"from":0,"to":1,"s":"accent","fx":"unknown"}]),
            json!([{"from":1,"to":2,"s":"accent"}]),
        ] {
            assert!(prepare("😀", 0, Some(&decor), None).is_err());
        }
    }
}

#[cfg(test)]
mod syntax_tests {
    use super::prepare;
    use crate::syntax_lite::TokenKind;

    #[test]
    fn code_highlighting_preserves_source_and_multiline_comment_state() {
        let Ok(drawing) = prepare(
            "/* comment\ncontinued */ let name = 3;",
            0,
            None,
            Some("rust"),
        ) else {
            panic!("code drawing");
        };
        assert_eq!(
            drawing
                .after
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "/* comment\ncontinued */ let name = 3;"
        );
        assert!(
            drawing
                .after
                .iter()
                .any(|run| run.syntax == TokenKind::Comment && run.text.contains("continued"))
        );
        assert!(
            drawing
                .after
                .iter()
                .any(|run| run.syntax == TokenKind::Keyword && run.text == "let")
        );
    }
}
