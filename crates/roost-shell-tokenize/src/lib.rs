//! Quote-aware shell tokenization shared by admission and worker command guards.
//! Guard entry points retain strict malformed-input and brace-boundary behavior.
//! Approval segmentation follows OMP's operator set and drops empty quoted words.

#![forbid(unsafe_code)]

mod javascript_whitespace;
pub use javascript_whitespace::{is_javascript_whitespace, is_javascript_whitespace_code_unit};

/// Splits a guarded script at unquoted command separators, including braces.
pub fn split_commands(source: &str) -> Option<Vec<String>> {
    split_command_text(source, true)
}

/// Splits one guarded command into words; malformed quoting or escapes refuse parsing.
pub fn tokenize(source: &str) -> Option<Vec<String>> {
    tokenize_words(source, true)
}

/// OMP approval segments retain partial words and omit empty quoted arguments.
pub fn tokenize_shell_segments(source: &str) -> Option<Vec<Vec<String>>> {
    split_command_text(source, false)?
        .into_iter()
        .map(|segment| tokenize_words(&segment, false))
        .collect::<Option<Vec<_>>>()
        .map(|segments| {
            segments
                .into_iter()
                .filter(|words| !words.is_empty())
                .collect()
        })
}

fn split_command_text(source: &str, guard_syntax: bool) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            current.push(character);
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            current.push(character);
            continue;
        }
        if quote.is_none()
            && (matches!(character, ';' | '\n' | '&' | '|' | '(' | ')')
                || (guard_syntax && matches!(character, '{' | '}')))
        {
            if !(if guard_syntax {
                current.trim().is_empty()
            } else {
                current.is_empty()
            }) {
                segments.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(character);
    }
    if guard_syntax && quote.is_some() {
        return None;
    }
    if !(if guard_syntax {
        current.trim().is_empty()
    } else {
        current.is_empty()
    }) {
        segments.push(current);
    }
    Some(segments)
}

fn tokenize_words(source: &str, guard_syntax: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut word_started = false;
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            if quote == Some('"') && !matches!(character, '"' | '\\' | '$' | '`') {
                word.push('\\');
            }
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            word_started = true;
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            word_started = true;
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                word.push(character);
            }
            continue;
        }
        let whitespace = if guard_syntax {
            character.is_whitespace()
        } else {
            matches!(character, ' ' | '\t')
        };
        if whitespace && quote.is_none() {
            if word_started {
                if guard_syntax || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                word_started = false;
            }
        } else {
            word_started = true;
            word.push(character);
        }
    }
    if guard_syntax && (quote.is_some() || escaped) {
        return None;
    }
    if escaped {
        word.push('\\');
    }
    if word_started && (guard_syntax || !word.is_empty()) {
        words.push(word);
    }
    Some(words)
}
