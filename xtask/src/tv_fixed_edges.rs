//! Fixed-position edge offsets in shared styles need a TV-scoped overscan
//! override: television browsers report safe-area insets as zero. This check
//! compares those declarations with `tv.css` and keeps intentional exceptions
//! explicit and local to their selector/property.

use crate::ratchet::CheckOutcome;
use crate::source_tree;
use crate::violation::Violation;

const STYLE_ROOT: &str = "crates/roost-web/assets/styles/";
const RULE: &str = "fixed-position edge offsets need a TV overscan override";
const MEMORY: &str = "docs/FAILURE-INDEX.md — env(safe-area-inset-*) is 0px on a television";

struct Allowance {
    selector: &'static str,
    property: &'static str,
    reason: &'static str,
}

// The input remains focusable for IME delivery while parked far outside the
// viewport; terminal navigation is repositioned into the composer tray on TV.
const ALLOWLIST: [Allowance; 3] = [
    Allowance {
        selector: ".terminal-input",
        property: "left",
        reason: "focusable IME input is intentionally parked outside the viewport",
    },
    Allowance {
        selector: ".term-nav",
        property: "right",
        reason: "TV terminal navigation is absolutely positioned inside the composer tray",
    },
    Allowance {
        selector: ".term-nav",
        property: "bottom",
        reason: "TV terminal navigation is absolutely positioned inside the composer tray",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Declaration {
    property: String,
    value: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Rule {
    selector: String,
    line: usize,
    declarations: Vec<Declaration>,
}

fn strip_comments(css: &str) -> String {
    let mut output = String::with_capacity(css.len());
    let mut chars = css.char_indices().peekable();
    while let Some((_, character)) = chars.next() {
        if character == '/' && chars.peek().is_some_and(|(_, next)| *next == '*') {
            chars.next();
            while let Some((_, current)) = chars.next() {
                if current == '*' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                    chars.next();
                    break;
                }
                if current == '\n' {
                    output.push('\n');
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

/// Read ordinary declaration blocks, including rules nested under at-rules.
/// The stylesheets use flat selector blocks; the last opening brace before a
/// close is therefore the declaration block even inside a media query.
fn parse_rules(css: &str) -> Vec<Rule> {
    let uncommented = strip_comments(css);
    let mut rules = Vec::new();
    let mut block_start = 0;
    while let Some(relative_end) = uncommented[block_start..].find('}') {
        let end = block_start + relative_end;
        let section = &uncommented[block_start..end];
        if let Some(open) = section.rfind('{') {
            let declarations_text = &section[open + 1..];
            let raw_prelude = section[..open].trim();
            let prelude = raw_prelude
                .rsplit_once('{')
                .map_or(raw_prelude, |(_, selector)| selector.trim());
            if !prelude.is_empty() && !prelude.starts_with('@') {
                let line = uncommented[..block_start + open].matches('\n').count() + 1;
                let declarations: Vec<Declaration> = declarations_text
                    .split(';')
                    .filter_map(|declaration| {
                        let (property, value) = declaration.split_once(':')?;
                        let property = property.trim().to_ascii_lowercase();
                        if property.is_empty() {
                            return None;
                        }
                        Some(Declaration {
                            property,
                            value: value.trim().to_ascii_lowercase(),
                        })
                    })
                    .collect();
                for selector in prelude
                    .split(',')
                    .map(str::trim)
                    .filter(|selector| !selector.is_empty())
                {
                    rules.push(Rule {
                        selector: selector.to_owned(),
                        line,
                        declarations: declarations.clone(),
                    });
                }
            }
        }
        block_start = end + 1;
    }
    rules
}

fn is_zero(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    value.split_whitespace().all(|part| {
        if matches!(part, "auto" | "initial" | "unset") {
            return true;
        }
        let number: String = part
            .chars()
            .take_while(|character| {
                character.is_ascii_digit() || *character == '.' || *character == '-'
            })
            .collect();
        !number.is_empty()
            && number
                .trim_start_matches('-')
                .trim_start_matches('.')
                .chars()
                .all(|character| character == '0')
            && part[number.len()..]
                .chars()
                .all(|character| character.is_ascii_alphabetic() || character == '%')
    })
}

fn required_token(property: &str) -> Option<&'static str> {
    match property {
        "left" | "right" | "inset-inline" | "inset-inline-start" | "inset-inline-end" => {
            Some("--tv-overscan-inline")
        }
        "top" | "bottom" | "inset-block" | "inset-block-start" | "inset-block-end" => {
            Some("--tv-overscan-block")
        }
        "inset" => Some("--tv-overscan-"),
        _ => None,
    }
}

fn edge_declarations(rule: &Rule) -> impl Iterator<Item = &Declaration> {
    rule.declarations.iter().filter(|declaration| {
        matches!(
            declaration.property.as_str(),
            "left"
                | "right"
                | "top"
                | "bottom"
                | "inset"
                | "inset-inline"
                | "inset-inline-start"
                | "inset-inline-end"
                | "inset-block"
                | "inset-block-start"
                | "inset-block-end"
        ) && !is_zero(&declaration.value)
    })
}

fn tv_selector_matches(source: &str, tv: &str) -> bool {
    let Some(scoped) = tv.strip_prefix("[data-tv=\"true\"] ") else {
        return false;
    };
    scoped == source
}

fn has_tv_override(source: &Rule, property: &str, token: &str, tv_rules: &[Rule]) -> bool {
    tv_rules.iter().any(|tv| {
        tv_selector_matches(&source.selector, &tv.selector)
            && tv.declarations.iter().any(|declaration| {
                declaration.property == property
                    && if token == "--tv-overscan-" {
                        declaration.value.contains("--tv-overscan-inline")
                            && declaration.value.contains("--tv-overscan-block")
                    } else {
                        declaration.value.contains(token)
                    }
            })
    })
}

fn allowance_reason(selector: &str, property: &str) -> Option<&'static str> {
    ALLOWLIST
        .iter()
        .find(|entry| entry.selector == selector && entry.property == property)
        .map(|entry| entry.reason)
}

fn inspect(source_rules: &[Rule], tv_rules: &[Rule], file: &str) -> Vec<Violation> {
    source_rules
        .iter()
        .filter(|rule| {
            rule.declarations.iter().any(|declaration| {
                declaration.property == "position" && declaration.value == "fixed"
            })
        })
        .flat_map(|rule| {
            edge_declarations(rule).filter_map(move |declaration| {
                let token = required_token(&declaration.property)?;
                if allowance_reason(&rule.selector, &declaration.property).is_some()
                    || has_tv_override(rule, &declaration.property, token, tv_rules)
                {
                    return None;
                }
                Some(Violation::new(
                    file,
                    rule.line,
                    format!(
                        "{} has nonzero {}: {} without a TV overscan override",
                        rule.selector, declaration.property, declaration.value
                    ),
                    RULE,
                    MEMORY,
                ))
            })
        })
        .collect()
}

fn is_style_file(relative: &str) -> bool {
    relative.starts_with(STYLE_ROOT)
        && !relative[STYLE_ROOT.len()..].contains('/')
        && relative.ends_with(".css")
        && relative != "crates/roost-web/assets/styles/tv.css"
        && relative != "crates/roost-web/assets/styles/theme-vars.css"
}

pub fn run() -> CheckOutcome {
    let root = source_tree::repo_root();
    let paths: Vec<_> = source_tree::walk(&root)
        .into_iter()
        .filter(|path| is_style_file(&source_tree::repo_relative(path)))
        .collect();
    let tv_path = root.join(format!("{STYLE_ROOT}tv.css"));
    let tv_rules = source_tree::read_text(&tv_path)
        .map(|css| parse_rules(&css))
        .unwrap_or_default();
    let mut violations = Vec::new();
    let mut checked = 0;
    for path in paths {
        let Some(css) = source_tree::read_text(&path) else {
            continue;
        };
        checked += 1;
        let relative = source_tree::repo_relative(&path);
        violations.extend(inspect(&parse_rules(&css), &tv_rules, &relative));
    }
    CheckOutcome { checked, violations }
}

#[cfg(test)]
mod tests {
    use super::{has_tv_override, inspect, is_style_file, parse_rules};

    #[test]
    fn parser_keeps_fixed_rule_declarations_and_selector() {
        let rules = parse_rules(".dock { position: fixed; right: 8px; top: 0; }");
        assert_eq!(rules[0].selector, ".dock");
        assert_eq!(rules[0].line, 1);
        assert!(
            rules[0]
                .declarations
                .iter()
                .any(|item| item.property == "right" && item.value == "8px")
        );
    }

    #[test]
    fn parser_reads_rules_inside_media_queries() {
        let rules = parse_rules(
            "@media (max-width: 600px) {\n  .dock {\n    position: fixed;\n    right: 8px;\n  }\n}",
        );
        assert_eq!(rules[0].selector, ".dock");
        assert!(
            rules[0]
                .declarations
                .iter()
                .any(|item| item.property == "right")
        );
    }

    #[test]
    fn zero_edges_need_no_override_but_nonzero_edges_do() {
        let source =
            parse_rules(".drawer { position: fixed; inset: 0; right: 0px; bottom: 12px; }");
        let tv =
            parse_rules("[data-tv=\"true\"] .drawer { bottom: var(--tv-overscan-block); }");
        assert!(has_tv_override(&source[0], "bottom", "--tv-overscan-block", &tv));
        assert!(inspect(&source, &tv, "shared.css").is_empty());
    }

    #[test]
    fn missing_or_unscoped_override_is_reported() {
        let source = parse_rules(".dock { position: fixed; right: 8px; }");
        let unscoped = parse_rules(".dock { right: var(--tv-overscan-inline); }");
        assert_eq!(inspect(&source, &unscoped, "shared.css").len(), 1);
        let wrong_token =
            parse_rules("[data-tv=\"true\"] .dock { right: var(--tv-overscan-block); }");
        assert_eq!(inspect(&source, &wrong_token, "shared.css").len(), 1);
        let different_selector =
            parse_rules("[data-tv=\"true\"] .dockyard { right: var(--tv-overscan-inline); }");
        assert_eq!(inspect(&source, &different_selector, "shared.css").len(), 1);
    }

    #[test]
    fn excludes_tokens_tv_stylesheet_and_nested_files() {
        assert!(!is_style_file("crates/roost-web/assets/styles/tv.css"));
        assert!(!is_style_file("crates/roost-web/assets/styles/theme-vars.css"));
        assert!(!is_style_file("crates/roost-web/assets/styles/nested/other.css"));
        assert!(is_style_file("crates/roost-web/assets/styles/sidebar.css"));
    }
}
