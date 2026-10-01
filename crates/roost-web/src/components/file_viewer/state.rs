//! What one read of one file means: the states the sheet can be in, and the
//! rules that turn bytes — or a refusal — into one. Owned by
//! `components::file_viewer`; read by `file_viewer::body` (the text) and
//! `file_viewer::states` (everything that is not text). Pure, so every rule
//! here is reachable from a test without a document or a coordinator.
//!
//! The size ceiling lives here because it is a decision about CONTENT, not
//! about the sheet: a file is text, a folder is a refusal, and both are decided
//! from the same answer.

use roost_client_core::client::rpc::CallError;
use roost_client_core::client::rpc::calls::browse::ReadFileContents;

use crate::platform::worker_paths::worker_path_basename;
use crate::syntax_lite::{Token, TokenKind, ext_from_basename, should_highlight, tokenize_lines};

/// The largest file the sheet will decode and paint. Past this the sheet is a
/// download rather than a preview: every line becomes a DOM row, and a reader
/// who wants two million bytes wants their editor.
pub const MAX_VIEWED_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// What the sheet is showing for the file it was pointed at.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerContent {
    /// Nothing has been asked for: the route named no file, or the file's
    /// machine is not in scope.
    Idle,
    /// A read is in flight.
    Loading,
    /// The file is text, and small enough to paint line by line.
    Text {
        /// The file's lines, in order.
        lines: Vec<String>,
        /// One token grid per line, for a highlighted extension; `None` when
        /// the file is painted as plain text.
        tokens: Option<Vec<Vec<Token>>>,
        /// The size the worker reported.
        byte_size: u64,
    },
    /// The bytes are not UTF-8, so there is no text to show.
    Binary {
        /// The size the worker reported.
        byte_size: u64,
    },
    /// The file holds nothing but the line breaks that end its lines.
    Empty,
    /// Refused before its lines were painted.
    TooLarge {
        /// The size the worker reported, which is what refused it.
        byte_size: u64,
    },
    /// The path is a folder, and the machine said so.
    Directory {
        /// The machine's own words.
        message: String,
    },
    /// The read failed, and the machine named why.
    Failed {
        /// The failure as the reader is shown it.
        message: String,
    },
}

impl ViewerContent {
    /// The state as a log field, so a read that lands says which one it was.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Text { .. } => "text",
            Self::Binary { .. } => "binary",
            Self::Empty => "empty",
            Self::TooLarge { .. } => "too-large",
            Self::Directory { .. } => "directory",
            Self::Failed { .. } => "failed",
        }
    }

    /// The file's size as the worker saw it, when the read produced one.
    #[must_use]
    pub fn byte_size(&self) -> u64 {
        match self {
            Self::Text { byte_size, .. }
            | Self::Binary { byte_size }
            | Self::TooLarge { byte_size } => *byte_size,
            _ => 0,
        }
    }

    /// Whether a read is in flight.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }
}

/// The state one read produced. `file_path` names the file the bytes came from,
/// and decides whether they are highlighted.
#[must_use]
pub fn classify(answer: Result<ReadFileContents, CallError>, file_path: &str) -> ViewerContent {
    match answer {
        Ok(contents) => classify_bytes(contents.data, contents.size, file_path),
        Err(error) => classify_failure(&error.to_string()),
    }
}

/// The state one answer's bytes produce.
#[must_use]
pub fn classify_bytes(data: Vec<u8>, byte_size: u64, file_path: &str) -> ViewerContent {
    if byte_size > MAX_VIEWED_FILE_BYTES {
        return ViewerContent::TooLarge { byte_size };
    }
    // A fatal decode, the same rule v2's `TextDecoder` was given: a file that
    // is not UTF-8 has no lines to number, and a lossy decode would invent them.
    let Ok(text) = String::from_utf8(data) else {
        return ViewerContent::Binary { byte_size };
    };
    if holds_no_text(&text) {
        return ViewerContent::Empty;
    }
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    ViewerContent::Text {
        tokens: highlighted_tokens(&lines, file_path),
        lines,
        byte_size,
    }
}

/// The state one refusal produces.
#[must_use]
pub fn classify_failure(message: &str) -> ViewerContent {
    if reads_as_directory(message) {
        ViewerContent::Directory {
            message: message.to_owned(),
        }
    } else {
        ViewerContent::Failed {
            message: message.to_owned(),
        }
    }
}

/// Whether a refusal means the path is a folder. The worker's own wording is
/// the only evidence there is: a machine that cannot be reached, a path that
/// does not exist, and a folder all fail the same read differently.
#[must_use]
pub fn reads_as_directory(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("directory") || lowered.contains("not a file")
}

/// One token grid per line, for a file whose extension is highlighted.
fn highlighted_tokens(lines: &[String], file_path: &str) -> Option<Vec<Vec<Token>>> {
    // The path codec owns what a basename is on every platform, so a Windows
    // worker's `C:\dir\file.rs` is classified by its file and not its drive.
    let basename = worker_path_basename(None, file_path).unwrap_or_else(|| file_path.to_owned());
    should_highlight(&ext_from_basename(&basename))
        .then(|| tokenize_lines(lines.iter().map(String::as_str)))
}

/// Whether a decoded file holds nothing a reader could read: no bytes at all,
/// or nothing but the line breaks that end its lines.
fn holds_no_text(text: &str) -> bool {
    text.bytes().all(|byte| byte == b'\n')
}

/// The `--syntax-*` token a kind of run is painted in.
#[must_use]
pub fn token_color(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Keyword => "var(--syntax-keyword)",
        TokenKind::String => "var(--syntax-string)",
        TokenKind::Comment => "var(--syntax-comment)",
        TokenKind::Number => "var(--syntax-number)",
        TokenKind::Plain => "var(--syntax-plain)",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_VIEWED_FILE_BYTES, ViewerContent, classify, classify_bytes, classify_failure,
        reads_as_directory, token_color,
    };
    use crate::syntax_lite::TokenKind;

    #[test]
    fn utf8_text_becomes_one_entry_per_line() {
        let state = classify_bytes(b"alpha\nbeta\n".to_vec(), 11, "/tmp/notes.txt");
        match state {
            ViewerContent::Text {
                lines, byte_size, ..
            } => {
                assert_eq!(
                    lines,
                    vec!["alpha".to_owned(), "beta".to_owned(), String::new()]
                );
                assert_eq!(byte_size, 11);
            }
            other => panic!("expected text, got {}", other.name()),
        }
    }

    #[test]
    fn a_highlighted_extension_gets_a_token_grid_and_a_plain_one_does_not() {
        let highlighted = classify_bytes(b"let x = 1;\n".to_vec(), 10, "/tmp/main.rs");
        let plain = classify_bytes(b"let x = 1;\n".to_vec(), 10, "/tmp/notes.md");
        assert!(matches!(
            highlighted,
            ViewerContent::Text {
                tokens: Some(_),
                ..
            }
        ));
        assert!(matches!(plain, ViewerContent::Text { tokens: None, .. }));
    }

    #[test]
    fn bytes_that_are_not_utf8_are_binary_and_keep_their_size() {
        let state = classify_bytes(vec![0xff, 0xfe, 0x00], 3, "/tmp/blob.bin");
        assert!(matches!(state, ViewerContent::Binary { byte_size: 3 }));
    }

    #[test]
    fn a_file_holding_nothing_but_line_breaks_is_empty() {
        for bytes in [Vec::new(), b"\n".to_vec(), b"\n\n".to_vec()] {
            let size = u64::try_from(bytes.len()).unwrap_or_default();
            let state = classify_bytes(bytes, size, "/tmp/empty.txt");
            assert!(matches!(state, ViewerContent::Empty), "empty was not empty");
        }
    }

    #[test]
    fn a_file_over_the_ceiling_is_refused_with_its_own_size() {
        let size = MAX_VIEWED_FILE_BYTES + 1;
        let state = classify_bytes(b"small".to_vec(), size, "/tmp/huge.txt");
        assert!(matches!(state, ViewerContent::TooLarge { byte_size } if byte_size == size));
    }

    #[test]
    fn a_folder_is_offered_where_a_refusal_otherwise_names_the_failure() {
        let directory = classify_failure("rpc error: Is a directory (os error 21)");
        let refused = classify_failure("rpc error: Permission denied (os error 13)");
        assert!(matches!(directory, ViewerContent::Directory { .. }));
        assert!(matches!(refused, ViewerContent::Failed { .. }));
    }

    #[test]
    fn a_refusal_naming_a_directory_answers_in_the_workers_own_words() {
        // `file_size`'s own pre-check, and the `io::Error` a read of a folder
        // produces, are the two sentences a machine actually sends.
        assert!(reads_as_directory("/tmp/notes is a directory"));
        assert!(reads_as_directory(
            "/tmp/notes: Is a directory (os error 21)"
        ));
        assert!(reads_as_directory("not a file"));
        assert!(!reads_as_directory("worker offline"));
    }

    #[test]
    fn a_successful_read_classifies_through_the_same_rules() {
        let answer = roost_client_core::client::rpc::calls::browse::ReadFileContents {
            data: b"one\n".to_vec(),
            size: 4,
        };
        assert!(matches!(
            classify(Ok(answer), "/tmp/one.txt"),
            ViewerContent::Text { .. }
        ));
    }

    #[test]
    fn only_the_size_and_the_read_answer_the_header_carry_a_size() {
        assert_eq!(
            ViewerContent::Text {
                lines: Vec::new(),
                tokens: None,
                byte_size: 9,
            }
            .byte_size(),
            9
        );
        assert_eq!(ViewerContent::Empty.byte_size(), 0);
        assert_eq!(ViewerContent::Loading.byte_size(), 0);
    }

    #[test]
    fn every_token_kind_paints_in_its_own_token() {
        let colors = [
            token_color(TokenKind::Keyword),
            token_color(TokenKind::String),
            token_color(TokenKind::Comment),
            token_color(TokenKind::Number),
            token_color(TokenKind::Plain),
        ];
        let named: Vec<&str> = colors
            .iter()
            .map(|color| {
                color
                    .trim_start_matches("var(--syntax-")
                    .trim_end_matches(')')
            })
            .collect();
        assert_eq!(named, ["keyword", "string", "comment", "number", "plain"]);
    }
}
