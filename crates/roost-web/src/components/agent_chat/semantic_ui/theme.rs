//! Scoped OMP theme variables and complete literal Tailwind token recipes.
//! The native surface inherits the actual source palette; Roost's app theme
//! remains unchanged, including when another conversation uses another theme.
use roost_protocol::wire::agent_chat::semantic_ui::palette::{valid_color, valid_token};
use std::collections::BTreeMap;
use std::fmt::Write;

pub(super) fn surface_style(palette: &BTreeMap<String, String>) -> String {
    let mut style = String::new();
    for (token, color) in palette {
        if valid_token(token) && valid_color(color) {
            let _ = write!(style, "--omp-theme-{token}:{color};");
        }
    }
    for (syntax, token) in [
        ("comment", "syntaxComment"),
        ("keyword", "syntaxKeyword"),
        ("function", "syntaxFunction"),
        ("variable", "syntaxVariable"),
        ("string", "syntaxString"),
        ("number", "syntaxNumber"),
        ("type", "syntaxType"),
        ("operator", "syntaxOperator"),
        ("punctuation", "syntaxPunctuation"),
    ] {
        let _ = write!(
            style,
            "--syntax-{syntax}:var(--omp-theme-{token},var(--term-fg));"
        );
    }
    style.push_str("color:var(--omp-theme-text,var(--term-fg));background-color:var(--term-bg);");
    style
}

pub(super) fn token_class(token: &str) -> Option<&'static str> {
    Some(match token {
        "accent" => "text-omp-accent",
        "border" => "text-omp-border",
        "borderAccent" => "text-omp-border-accent",
        "borderMuted" => "text-omp-border-muted",
        "success" => "text-omp-success",
        "error" => "text-omp-error",
        "warning" => "text-omp-warning",
        "muted" => "text-omp-muted",
        "dim" => "text-omp-dim",
        "text" => "text-omp-text",
        "assistantMessageText" => "text-omp-assistant-message-text",
        "thinkingText" => "text-omp-thinking-text",
        "userMessageText" => "text-omp-user-message-text",
        "customMessageText" => "text-omp-custom-message-text",
        "customMessageLabel" => "text-omp-custom-message-label",
        "toolTitle" => "text-omp-tool-title",
        "toolOutput" => "text-omp-tool-output",
        "mdHeading" => "text-omp-md-heading",
        "mdLink" => "text-omp-md-link",
        "mdLinkUrl" => "text-omp-md-link-url",
        "mdCode" => "text-omp-md-code",
        "mdCodeBlock" => "text-omp-md-code-block",
        "mdCodeBlockBorder" => "text-omp-md-code-block-border",
        "mdQuote" => "text-omp-md-quote",
        "mdQuoteBorder" => "text-omp-md-quote-border",
        "mdHr" => "text-omp-md-hr",
        "mdListBullet" => "text-omp-md-list-bullet",
        "toolDiffAdded" => "text-omp-tool-diff-added",
        "toolDiffRemoved" => "text-omp-tool-diff-removed",
        "toolDiffContext" => "text-omp-tool-diff-context",
        "syntaxComment" => "text-omp-syntax-comment",
        "syntaxKeyword" => "text-omp-syntax-keyword",
        "syntaxFunction" => "text-omp-syntax-function",
        "syntaxVariable" => "text-omp-syntax-variable",
        "syntaxString" => "text-omp-syntax-string",
        "syntaxNumber" => "text-omp-syntax-number",
        "syntaxType" => "text-omp-syntax-type",
        "syntaxOperator" => "text-omp-syntax-operator",
        "syntaxPunctuation" => "text-omp-syntax-punctuation",
        "thinkingOff" => "text-omp-thinking-off",
        "thinkingMinimal" => "text-omp-thinking-minimal",
        "thinkingLow" => "text-omp-thinking-low",
        "thinkingMedium" => "text-omp-thinking-medium",
        "thinkingHigh" => "text-omp-thinking-high",
        "thinkingXhigh" => "text-omp-thinking-xhigh",
        "thinkingMax" => "text-omp-thinking-max",
        "bashMode" => "text-omp-bash-mode",
        "pythonMode" => "text-omp-python-mode",
        "statusLineSep" => "text-omp-status-line-sep",
        "statusLineModel" => "text-omp-status-line-model",
        "statusLinePath" => "text-omp-status-line-path",
        "statusLineGitClean" => "text-omp-status-line-git-clean",
        "statusLineGitDirty" => "text-omp-status-line-git-dirty",
        "statusLineContext" => "text-omp-status-line-context",
        "statusLineSpend" => "text-omp-status-line-spend",
        "statusLineStaged" => "text-omp-status-line-staged",
        "statusLineDirty" => "text-omp-status-line-dirty",
        "statusLineUntracked" => "text-omp-status-line-untracked",
        "statusLineOutput" => "text-omp-status-line-output",
        "statusLineCost" => "text-omp-status-line-cost",
        "statusLineSubagents" => "text-omp-status-line-subagents",
        "bg:selectedBg" => "bg-omp-selected-bg",
        "bg:userMessageBg" => "bg-omp-user-message-bg",
        "bg:customMessageBg" => "bg-omp-custom-message-bg",
        "bg:toolPendingBg" => "bg-omp-tool-pending-bg",
        "bg:toolSuccessBg" => "bg-omp-tool-success-bg",
        "bg:toolErrorBg" => "bg-omp-tool-error-bg",
        "bg:statusLineBg" => "bg-omp-status-line-bg",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_palette_is_scoped_and_background_tokens_are_not_roost_muted() {
        let color = format!("#{:06x}", 0x221d1a);
        let palette = BTreeMap::from([("userMessageBg".into(), color.clone())]);
        let style = surface_style(&palette);
        assert!(style.contains(&format!("--omp-theme-userMessageBg:{color};")));
        assert!(!style.contains("--color-muted:"));
        assert_eq!(
            token_class("bg:userMessageBg"),
            Some("bg-omp-user-message-bg")
        );
        assert_eq!(token_class("unknown"), None);
        assert!(!surface_style(&BTreeMap::new()).contains(&color));
    }
}
