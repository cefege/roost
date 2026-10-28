//! Code detection and tokenization for tool output and written files.
//! Markdown code fences use the same tokenizer; terminal ANSI output stays in
//! `output_lines` because its palette comes from process-emitted SGR codes.

/// Tokens for code output, while prose keeps its terminal-style plain text.
pub(super) fn syntax_tokens<'a>(
    lines: impl Iterator<Item = &'a str> + Clone,
    language: Option<&str>,
) -> Option<Vec<Vec<crate::syntax_lite::Token>>> {
    if lines.clone().any(|line| line.contains('\u{1b}')) {
        return None;
    }
    let detected_code = language.is_some_and(crate::syntax_lite::should_highlight)
        || lines.clone().any(|line| {
            [
                "fn ",
                "let ",
                "const ",
                "import ",
                "export ",
                "return ",
                "def ",
                "class ",
                "function ",
                "=>",
                "if (",
                "while (",
                "echo ",
                "printf ",
            ]
            .iter()
            .any(|marker| line.contains(marker))
        });
    detected_code.then(|| crate::syntax_lite::tokenize_lines(lines))
}

#[cfg(test)]
mod tests {
    use super::syntax_tokens;

    #[test]
    fn code_output_gets_syntax_classes_but_prose_does_not() {
        let code = syntax_tokens("let count = 3;".lines(), None).unwrap_or_default();
        let keyword = code.first().and_then(|tokens| {
            tokens
                .iter()
                .find(|token| token.kind == crate::syntax_lite::TokenKind::Keyword)
                .map(|token| token.kind)
        });
        assert_eq!(keyword, Some(crate::syntax_lite::TokenKind::Keyword));
        assert!(syntax_tokens("the process completed successfully".lines(), None).is_none());
    }

    #[test]
    fn highlighting_keeps_the_output_sanitization_and_line_limit() {
        let output = format!("discarded\rlet text = \"{}\";\u{0007}", "x".repeat(5_000));
        let lines = super::super::output_lines::styled_lines(&output, false);
        let rendered = || {
            lines
                .iter()
                .map(|line| line.runs.first().map_or("", |run| run.text.as_str()))
        };
        let tokens = syntax_tokens(rendered(), Some("rust")).unwrap_or_default();
        let highlighted: String = tokens
            .into_iter()
            .flatten()
            .map(|token| token.text)
            .collect();
        let expected: String = rendered().collect();
        assert_eq!(highlighted, expected);
        assert!(highlighted.starts_with("let text = "));
        assert!(highlighted.ends_with('…'));
        assert!(!highlighted.chars().any(char::is_control));
    }
}
