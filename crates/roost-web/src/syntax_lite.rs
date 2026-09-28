//! A minimal line-by-line syntax highlighter: keywords, strings, comments and
//! numbers over a JS/Go/Python/Rust/shell keyword set, with block-comment state
//! carried across lines. Ports `apps/web/src/lib/syntaxLite.ts`; read by the
//! file viewer sheet (BROWSE), which maps each kind to a `--syntax-<kind>` token.

use std::collections::BTreeSet;

/// What a token is, for colouring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    String,
    Comment,
    Number,
    Plain,
}

impl TokenKind {
    /// The `--syntax-<kind>` suffix.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::String => "string",
            Self::Comment => "comment",
            Self::Number => "number",
            Self::Plain => "plain",
        }
    }
}

/// One coloured run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The text.
    pub text: String,
    /// Its colour.
    pub kind: TokenKind,
}

const KEYWORDS: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "import", "in",
    "instanceof", "interface", "let", "new", "null", "of", "return", "static", "super", "switch",
    "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "with",
    "yield", "async", "await", "enum", "implements", "package", "private", "protected", "public",
    "readonly", "abstract", "declare", "namespace", "func", "go", "defer", "chan", "select", "map",
    "range", "make", "len", "cap", "append", "copy", "close", "print", "println", "def", "lambda",
    "pass", "not", "and", "or", "is", "as", "global", "nonlocal", "assert", "raise", "except",
    "elif", "fn", "mut", "impl", "trait", "struct", "where", "mod", "use", "pub", "self", "crate",
    "move", "unsafe", "extern", "ref", "match", "loop", "dyn",
];

const HIGHLIGHTED_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "py", "rs", "c", "cpp", "h", "hpp", "java", "kt",
    "swift", "rb", "php", "sh", "bash", "zsh", "css", "scss", "less",
];

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
}

fn is_ident(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

fn is_number_body(ch: char) -> bool {
    ch.is_ascii_hexdigit() || matches!(ch, 'x' | 'X' | 'o' | 'O' | '_' | '.' | '+' | '-')
}

fn starts_comment(chars: &[char], at: usize) -> bool {
    chars.get(at) == Some(&'/') && matches!(chars.get(at + 1), Some('/') | Some('*'))
}

/// Tokenize one line; returns its tokens and whether a block comment is still
/// open at its end.
fn tokenize_line(line: &str, in_block: bool, keywords: &BTreeSet<&str>) -> (Vec<Token>, bool) {
    let chars: Vec<char> = line.chars().collect();
    let text = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut tokens = Vec::new();
    let mut push = |text: String, kind| {
        if !text.is_empty() {
            tokens.push(Token { text, kind });
        }
    };
    let find_close = |from: usize| (from..chars.len().saturating_sub(1)).find(|&at| chars[at] == '*' && chars[at + 1] == '/');
    let mut index = 0;
    if in_block {
        match find_close(0) {
            None => {
                push(line.to_owned(), TokenKind::Comment);
                return (tokens, true);
            }
            Some(close) => {
                push(text(0, close + 2), TokenKind::Comment);
                index = close + 2;
            }
        }
    }
    while index < chars.len() {
        let ch = chars[index];
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            match find_close(index + 2) {
                None => {
                    push(text(index, chars.len()), TokenKind::Comment);
                    return (tokens, true);
                }
                Some(close) => {
                    push(text(index, close + 2), TokenKind::Comment);
                    index = close + 2;
                    continue;
                }
            }
        }
        if (ch == '/' && chars.get(index + 1) == Some(&'/')) || ch == '#' {
            push(text(index, chars.len()), TokenKind::Comment);
            return (tokens, false);
        }
        let start = index;
        if matches!(ch, '"' | '\'' | '`') {
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' && index + 1 < chars.len() {
                    index += 2;
                    continue;
                }
                index += 1;
                if chars[index - 1] == ch {
                    break;
                }
            }
            push(text(start, index), TokenKind::String);
        } else if ch.is_ascii_digit() {
            while index < chars.len() && is_number_body(chars[index]) {
                index += 1;
            }
            push(text(start, index), TokenKind::Number);
        } else if is_ident_start(ch) {
            while index < chars.len() && is_ident(chars[index]) {
                index += 1;
            }
            let word = text(start, index);
            let kind = if keywords.contains(word.as_str()) { TokenKind::Keyword } else { TokenKind::Plain };
            push(word, kind);
        } else {
            index += 1;
            while index < chars.len()
                && !(is_ident(chars[index]) || matches!(chars[index], '"' | '\'' | '`' | '#'))
                && !starts_comment(&chars, index)
            {
                index += 1;
            }
            push(text(start, index), TokenKind::Plain);
        }
    }
    (tokens, false)
}

/// Tokenize every line of a file, carrying block-comment state across lines.
pub fn tokenize_lines<'line>(lines: impl IntoIterator<Item = &'line str>) -> Vec<Vec<Token>> {
    let keywords: BTreeSet<&str> = KEYWORDS.iter().copied().collect();
    let mut in_block = false;
    lines
        .into_iter()
        .map(|line| {
            let (tokens, open) = tokenize_line(line, in_block, &keywords);
            in_block = open;
            tokens
        })
        .collect()
}

/// Whether an extension gets highlighted (markdown, JSON and YAML do not).
pub fn should_highlight(ext: &str) -> bool {
    HIGHLIGHTED_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// A file's extension from its basename, lowercase, without the dot.
pub fn ext_from_basename(basename: &str) -> String {
    basename
        .rfind('.')
        .map(|dot| basename[dot + 1..].to_lowercase())
        .unwrap_or_default()
}
