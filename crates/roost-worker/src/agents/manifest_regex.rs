//! The regex dialect the pinned agent manifests are evaluated in. The patterns
//! are Herdr's Rust syntax, but v2 ran them as JavaScript `u`-flag regexes, so
//! the classes whose meaning differs between the engines are rewritten to
//! JavaScript's: ASCII `\w`/`\d`/`\b`, JavaScript's `\s` set, and a `.` that
//! stops at every line terminator. Ports `compileHerdrRegex` from
//! `apps/worker/src/agents/manifest-engine.ts`; called by `agents::manifest_engine`.

use regex::Regex;

/// ECMAScript `WhiteSpace` plus `LineTerminator`, as class members.
const JAVASCRIPT_WHITESPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";
const JAVASCRIPT_WORD: &str = "0-9A-Za-z_";
/// A JavaScript `.` without the `s` flag.
const JAVASCRIPT_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// Compile one pinned pattern with JavaScript's class semantics. `^`/`$`
/// already bind to the whole region in both engines: neither sets multi-line
/// mode, which is what keeps a trust prompt quoted mid-transcript from
/// satisfying an anchored blocker.
pub fn compile_herdr_regex(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&with_javascript_classes(pattern))
}

fn with_javascript_classes(pattern: &str) -> String {
    let mut rewritten = String::with_capacity(pattern.len() * 2);
    let mut characters = pattern.chars();
    let mut in_class = false;
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                let Some(escaped) = characters.next() else {
                    rewritten.push('\\');
                    break;
                };
                push_escape(&mut rewritten, escaped, in_class);
            }
            '[' if in_class => rewritten.push_str(r"\["),
            '&' | '~' if in_class => {
                // Class set operators in this engine, plain members in JavaScript.
                rewritten.push('\\');
                rewritten.push(character);
            }
            '[' => {
                in_class = true;
                rewritten.push('[');
            }
            ']' if in_class => {
                in_class = false;
                rewritten.push(']');
            }
            '.' if !in_class => rewritten.push_str(JAVASCRIPT_DOT),
            other => rewritten.push(other),
        }
    }
    rewritten
}

fn push_escape(rewritten: &mut String, escaped: char, in_class: bool) {
    let class =
        |members: &str, negated: bool| format!("[{}{members}]", if negated { "^" } else { "" });
    match (escaped, in_class) {
        ('w', true) => rewritten.push_str(JAVASCRIPT_WORD),
        ('d', true) => rewritten.push_str("0-9"),
        ('s', true) => rewritten.push_str(JAVASCRIPT_WHITESPACE),
        ('w', false) => rewritten.push_str("\\w"),
        ('d', false) => rewritten.push_str(&class("0-9", false)),
        ('s', false) => rewritten.push_str(&class(JAVASCRIPT_WHITESPACE, false)),
        ('W', _) => rewritten.push_str(&class(JAVASCRIPT_WORD, true)),
        ('D', _) => rewritten.push_str(&class("0-9", true)),
        ('S', _) => rewritten.push_str(&class(JAVASCRIPT_WHITESPACE, true)),
        ('b', false) => rewritten.push_str(r"(?-u:\b)"),
        (other, _) => {
            rewritten.push('\\');
            rewritten.push(other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compile_herdr_regex;

    fn matches(pattern: &str, haystack: &str) -> bool {
        compile_herdr_regex(pattern)
            .unwrap_or_else(|error| panic!("{pattern} compiles: {error}"))
            .is_match(haystack)
    }

    #[test]
    fn word_digit_and_boundary_classes_are_ascii_as_in_javascript() {
        assert!(!matches(r"^\w$", "é"));
        assert!(matches(r"^\w$", "e"));
        assert!(!matches(r"^\d$", "٣"));
        assert!(matches(r"ing\b", "Thinkingé"));
        assert!(matches(r"^[\w]+$", "abc_9"));
        assert!(!matches(r"^[\w]+$", "abé"));
    }

    #[test]
    fn dot_stops_at_every_javascript_line_terminator() {
        assert!(!matches(r"^a.b$", "a\rb"));
        assert!(!matches(r"^a.b$", "a\u{2028}b"));
        assert!(matches(r"^a.b$", "a b"));
        assert!(matches(r"^a[.]b$", "a.b"));
        assert!(!matches(r"^a[.]b$", "axb"));
    }

    #[test]
    fn whitespace_is_javascript_s_set() {
        assert!(matches(r"^\s$", "\u{feff}"));
        assert!(!matches(r"^\s$", "\u{85}"));
        assert!(matches(r"^\S$", "\u{85}"));
    }

    #[test]
    fn anchors_bind_to_the_whole_region_and_flags_carry_over() {
        assert!(!matches(r"^b$", "a\nb"));
        assert!(matches(r"(?i)^\s*allow .*\(y\)", "  ALLOW write (y)"));
        assert!(matches(r"^[\x{2800}-\x{28FF}] ", "⠋ task"));
        assert!(matches(r"^\s*[\u2800-\u28FF]", " ⠋"));
    }
}
