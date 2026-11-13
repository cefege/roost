//! Native tone or theme-colour token → the OMP palette colour the terminal draws it with.
//! Used by charts and meters so a bar, mark, part or stroke paints like its terminal glyph.
//! Every class is a complete literal because Tailwind reads classes from source.

/// Palette token, then its fill, border, stroke, svg fill and text classes.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// One row per `--color-omp-*` role.
#[rustfmt::skip]
const PALETTE: &[Row] = &[
    ("accent", "bg-omp-accent", "border-omp-accent", "stroke-omp-accent", "fill-omp-accent", "text-omp-accent"),
    ("assistantMessageText", "bg-omp-assistant-message-text", "border-omp-assistant-message-text", "stroke-omp-assistant-message-text", "fill-omp-assistant-message-text", "text-omp-assistant-message-text"),
    ("bashMode", "bg-omp-bash-mode", "border-omp-bash-mode", "stroke-omp-bash-mode", "fill-omp-bash-mode", "text-omp-bash-mode"),
    ("border", "bg-omp-border", "border-omp-border", "stroke-omp-border", "fill-omp-border", "text-omp-border"),
    ("borderAccent", "bg-omp-border-accent", "border-omp-border-accent", "stroke-omp-border-accent", "fill-omp-border-accent", "text-omp-border-accent"),
    ("borderMuted", "bg-omp-border-muted", "border-omp-border-muted", "stroke-omp-border-muted", "fill-omp-border-muted", "text-omp-border-muted"),
    ("customMessageBg", "bg-omp-custom-message-bg", "border-omp-custom-message-bg", "stroke-omp-custom-message-bg", "fill-omp-custom-message-bg", "text-omp-custom-message-bg"),
    ("customMessageLabel", "bg-omp-custom-message-label", "border-omp-custom-message-label", "stroke-omp-custom-message-label", "fill-omp-custom-message-label", "text-omp-custom-message-label"),
    ("customMessageText", "bg-omp-custom-message-text", "border-omp-custom-message-text", "stroke-omp-custom-message-text", "fill-omp-custom-message-text", "text-omp-custom-message-text"),
    ("dim", "bg-omp-dim", "border-omp-dim", "stroke-omp-dim", "fill-omp-dim", "text-omp-dim"),
    ("error", "bg-omp-error", "border-omp-error", "stroke-omp-error", "fill-omp-error", "text-omp-error"),
    ("mdCode", "bg-omp-md-code", "border-omp-md-code", "stroke-omp-md-code", "fill-omp-md-code", "text-omp-md-code"),
    ("mdCodeBlock", "bg-omp-md-code-block", "border-omp-md-code-block", "stroke-omp-md-code-block", "fill-omp-md-code-block", "text-omp-md-code-block"),
    ("mdCodeBlockBorder", "bg-omp-md-code-block-border", "border-omp-md-code-block-border", "stroke-omp-md-code-block-border", "fill-omp-md-code-block-border", "text-omp-md-code-block-border"),
    ("mdHeading", "bg-omp-md-heading", "border-omp-md-heading", "stroke-omp-md-heading", "fill-omp-md-heading", "text-omp-md-heading"),
    ("mdHr", "bg-omp-md-hr", "border-omp-md-hr", "stroke-omp-md-hr", "fill-omp-md-hr", "text-omp-md-hr"),
    ("mdLink", "bg-omp-md-link", "border-omp-md-link", "stroke-omp-md-link", "fill-omp-md-link", "text-omp-md-link"),
    ("mdLinkUrl", "bg-omp-md-link-url", "border-omp-md-link-url", "stroke-omp-md-link-url", "fill-omp-md-link-url", "text-omp-md-link-url"),
    ("mdListBullet", "bg-omp-md-list-bullet", "border-omp-md-list-bullet", "stroke-omp-md-list-bullet", "fill-omp-md-list-bullet", "text-omp-md-list-bullet"),
    ("mdQuote", "bg-omp-md-quote", "border-omp-md-quote", "stroke-omp-md-quote", "fill-omp-md-quote", "text-omp-md-quote"),
    ("mdQuoteBorder", "bg-omp-md-quote-border", "border-omp-md-quote-border", "stroke-omp-md-quote-border", "fill-omp-md-quote-border", "text-omp-md-quote-border"),
    ("muted", "bg-omp-muted", "border-omp-muted", "stroke-omp-muted", "fill-omp-muted", "text-omp-muted"),
    ("pythonMode", "bg-omp-python-mode", "border-omp-python-mode", "stroke-omp-python-mode", "fill-omp-python-mode", "text-omp-python-mode"),
    ("selectedBg", "bg-omp-selected-bg", "border-omp-selected-bg", "stroke-omp-selected-bg", "fill-omp-selected-bg", "text-omp-selected-bg"),
    ("statusLineBg", "bg-omp-status-line-bg", "border-omp-status-line-bg", "stroke-omp-status-line-bg", "fill-omp-status-line-bg", "text-omp-status-line-bg"),
    ("statusLineContext", "bg-omp-status-line-context", "border-omp-status-line-context", "stroke-omp-status-line-context", "fill-omp-status-line-context", "text-omp-status-line-context"),
    ("statusLineCost", "bg-omp-status-line-cost", "border-omp-status-line-cost", "stroke-omp-status-line-cost", "fill-omp-status-line-cost", "text-omp-status-line-cost"),
    ("statusLineDirty", "bg-omp-status-line-dirty", "border-omp-status-line-dirty", "stroke-omp-status-line-dirty", "fill-omp-status-line-dirty", "text-omp-status-line-dirty"),
    ("statusLineGitClean", "bg-omp-status-line-git-clean", "border-omp-status-line-git-clean", "stroke-omp-status-line-git-clean", "fill-omp-status-line-git-clean", "text-omp-status-line-git-clean"),
    ("statusLineGitDirty", "bg-omp-status-line-git-dirty", "border-omp-status-line-git-dirty", "stroke-omp-status-line-git-dirty", "fill-omp-status-line-git-dirty", "text-omp-status-line-git-dirty"),
    ("statusLineModel", "bg-omp-status-line-model", "border-omp-status-line-model", "stroke-omp-status-line-model", "fill-omp-status-line-model", "text-omp-status-line-model"),
    ("statusLineOutput", "bg-omp-status-line-output", "border-omp-status-line-output", "stroke-omp-status-line-output", "fill-omp-status-line-output", "text-omp-status-line-output"),
    ("statusLinePath", "bg-omp-status-line-path", "border-omp-status-line-path", "stroke-omp-status-line-path", "fill-omp-status-line-path", "text-omp-status-line-path"),
    ("statusLineSep", "bg-omp-status-line-sep", "border-omp-status-line-sep", "stroke-omp-status-line-sep", "fill-omp-status-line-sep", "text-omp-status-line-sep"),
    ("statusLineSpend", "bg-omp-status-line-spend", "border-omp-status-line-spend", "stroke-omp-status-line-spend", "fill-omp-status-line-spend", "text-omp-status-line-spend"),
    ("statusLineStaged", "bg-omp-status-line-staged", "border-omp-status-line-staged", "stroke-omp-status-line-staged", "fill-omp-status-line-staged", "text-omp-status-line-staged"),
    ("statusLineSubagents", "bg-omp-status-line-subagents", "border-omp-status-line-subagents", "stroke-omp-status-line-subagents", "fill-omp-status-line-subagents", "text-omp-status-line-subagents"),
    ("statusLineUntracked", "bg-omp-status-line-untracked", "border-omp-status-line-untracked", "stroke-omp-status-line-untracked", "fill-omp-status-line-untracked", "text-omp-status-line-untracked"),
    ("success", "bg-omp-success", "border-omp-success", "stroke-omp-success", "fill-omp-success", "text-omp-success"),
    ("syntaxComment", "bg-omp-syntax-comment", "border-omp-syntax-comment", "stroke-omp-syntax-comment", "fill-omp-syntax-comment", "text-omp-syntax-comment"),
    ("syntaxFunction", "bg-omp-syntax-function", "border-omp-syntax-function", "stroke-omp-syntax-function", "fill-omp-syntax-function", "text-omp-syntax-function"),
    ("syntaxKeyword", "bg-omp-syntax-keyword", "border-omp-syntax-keyword", "stroke-omp-syntax-keyword", "fill-omp-syntax-keyword", "text-omp-syntax-keyword"),
    ("syntaxNumber", "bg-omp-syntax-number", "border-omp-syntax-number", "stroke-omp-syntax-number", "fill-omp-syntax-number", "text-omp-syntax-number"),
    ("syntaxOperator", "bg-omp-syntax-operator", "border-omp-syntax-operator", "stroke-omp-syntax-operator", "fill-omp-syntax-operator", "text-omp-syntax-operator"),
    ("syntaxPunctuation", "bg-omp-syntax-punctuation", "border-omp-syntax-punctuation", "stroke-omp-syntax-punctuation", "fill-omp-syntax-punctuation", "text-omp-syntax-punctuation"),
    ("syntaxString", "bg-omp-syntax-string", "border-omp-syntax-string", "stroke-omp-syntax-string", "fill-omp-syntax-string", "text-omp-syntax-string"),
    ("syntaxType", "bg-omp-syntax-type", "border-omp-syntax-type", "stroke-omp-syntax-type", "fill-omp-syntax-type", "text-omp-syntax-type"),
    ("syntaxVariable", "bg-omp-syntax-variable", "border-omp-syntax-variable", "stroke-omp-syntax-variable", "fill-omp-syntax-variable", "text-omp-syntax-variable"),
    ("text", "bg-omp-text", "border-omp-text", "stroke-omp-text", "fill-omp-text", "text-omp-text"),
    ("thinkingHigh", "bg-omp-thinking-high", "border-omp-thinking-high", "stroke-omp-thinking-high", "fill-omp-thinking-high", "text-omp-thinking-high"),
    ("thinkingLow", "bg-omp-thinking-low", "border-omp-thinking-low", "stroke-omp-thinking-low", "fill-omp-thinking-low", "text-omp-thinking-low"),
    ("thinkingMax", "bg-omp-thinking-max", "border-omp-thinking-max", "stroke-omp-thinking-max", "fill-omp-thinking-max", "text-omp-thinking-max"),
    ("thinkingMedium", "bg-omp-thinking-medium", "border-omp-thinking-medium", "stroke-omp-thinking-medium", "fill-omp-thinking-medium", "text-omp-thinking-medium"),
    ("thinkingMinimal", "bg-omp-thinking-minimal", "border-omp-thinking-minimal", "stroke-omp-thinking-minimal", "fill-omp-thinking-minimal", "text-omp-thinking-minimal"),
    ("thinkingOff", "bg-omp-thinking-off", "border-omp-thinking-off", "stroke-omp-thinking-off", "fill-omp-thinking-off", "text-omp-thinking-off"),
    ("thinkingText", "bg-omp-thinking-text", "border-omp-thinking-text", "stroke-omp-thinking-text", "fill-omp-thinking-text", "text-omp-thinking-text"),
    ("thinkingXhigh", "bg-omp-thinking-xhigh", "border-omp-thinking-xhigh", "stroke-omp-thinking-xhigh", "fill-omp-thinking-xhigh", "text-omp-thinking-xhigh"),
    ("toolDiffAdded", "bg-omp-tool-diff-added", "border-omp-tool-diff-added", "stroke-omp-tool-diff-added", "fill-omp-tool-diff-added", "text-omp-tool-diff-added"),
    ("toolDiffContext", "bg-omp-tool-diff-context", "border-omp-tool-diff-context", "stroke-omp-tool-diff-context", "fill-omp-tool-diff-context", "text-omp-tool-diff-context"),
    ("toolDiffRemoved", "bg-omp-tool-diff-removed", "border-omp-tool-diff-removed", "stroke-omp-tool-diff-removed", "fill-omp-tool-diff-removed", "text-omp-tool-diff-removed"),
    ("toolErrorBg", "bg-omp-tool-error-bg", "border-omp-tool-error-bg", "stroke-omp-tool-error-bg", "fill-omp-tool-error-bg", "text-omp-tool-error-bg"),
    ("toolOutput", "bg-omp-tool-output", "border-omp-tool-output", "stroke-omp-tool-output", "fill-omp-tool-output", "text-omp-tool-output"),
    ("toolPendingBg", "bg-omp-tool-pending-bg", "border-omp-tool-pending-bg", "stroke-omp-tool-pending-bg", "fill-omp-tool-pending-bg", "text-omp-tool-pending-bg"),
    ("toolSuccessBg", "bg-omp-tool-success-bg", "border-omp-tool-success-bg", "stroke-omp-tool-success-bg", "fill-omp-tool-success-bg", "text-omp-tool-success-bg"),
    ("toolTitle", "bg-omp-tool-title", "border-omp-tool-title", "stroke-omp-tool-title", "fill-omp-tool-title", "text-omp-tool-title"),
    ("userMessageBg", "bg-omp-user-message-bg", "border-omp-user-message-bg", "stroke-omp-user-message-bg", "fill-omp-user-message-bg", "text-omp-user-message-bg"),
    ("userMessageText", "bg-omp-user-message-text", "border-omp-user-message-text", "stroke-omp-user-message-text", "fill-omp-user-message-text", "text-omp-user-message-text"),
    ("warning", "bg-omp-warning", "border-omp-warning", "stroke-omp-warning", "fill-omp-warning", "text-omp-warning"),
];

/// The theme token the terminal uses for `tone`, or a theme-colour token itself;
/// `fallback` when the node carries none.
fn palette_token<'a>(tone: Option<&'a str>, fallback: &'a str) -> &'a str {
    match tone.unwrap_or(fallback) {
        "neutral" => "text",
        "info" => "accent",
        "pending" => "warning",
        "user" => "customMessageLabel",
        "track" => "border",
        other => other,
    }
}

fn classes(tone: Option<&str>, fallback: &str) -> &'static Row {
    let token = palette_token(tone, fallback);
    PALETTE
        .iter()
        .find(|row| row.0 == token)
        .unwrap_or(&PALETTE[0])
}

pub(super) fn tone_fill(tone: Option<&str>, fallback: &str) -> &'static str {
    classes(tone, fallback).1
}

pub(super) fn tone_border(tone: Option<&str>, fallback: &str) -> &'static str {
    classes(tone, fallback).2
}

pub(super) fn tone_stroke(tone: Option<&str>, fallback: &str) -> &'static str {
    classes(tone, fallback).3
}

pub(super) fn tone_svg_fill(tone: Option<&str>, fallback: &str) -> &'static str {
    classes(tone, fallback).4
}

pub(super) fn tone_text(tone: Option<&str>, fallback: &str) -> &'static str {
    classes(tone, fallback).5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_resolve_to_the_terminal_palette_token() {
        assert_eq!(tone_fill(Some("pending"), "accent"), "bg-omp-warning");
        assert_eq!(tone_fill(Some("info"), "muted"), "bg-omp-accent");
        assert_eq!(
            tone_fill(Some("user"), "accent"),
            "bg-omp-custom-message-label"
        );
        assert_eq!(tone_fill(None, "muted"), "bg-omp-muted");
        assert_eq!(tone_border(Some("neutral"), "muted"), "border-omp-text");
        assert_eq!(tone_stroke(Some("error"), "muted"), "stroke-omp-error");
    }

    #[test]
    fn theme_colour_tokens_resolve_to_their_palette_role() {
        assert_eq!(tone_fill(None, "statusLineCost"), "bg-omp-status-line-cost");
        assert_eq!(tone_svg_fill(None, "mdLinkUrl"), "fill-omp-md-link-url");
    }
}
