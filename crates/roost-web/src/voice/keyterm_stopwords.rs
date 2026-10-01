//! The words keyterm biasing must never spend a term on.
//!
//! A terminal is dense with English and with the shell's own vocabulary, and a
//! keyterm for `error` or `return` costs URL bytes and buys nothing: the
//! recognizer already knows those words, and biasing them is how a bias list
//! stops being a signal. Data only, ported from
//! `apps/web/src/voice/keytermStopwords.ts`.

/// The stop set, lowercase.
const STOPWORDS: &[&str] = &[
    // English function words.
    "the", "a", "an", "and", "or", "but", "if", "then", "else", "for", "while", "do", "done", "to",
    "of", "in", "on", "at", "by", "with", "from", "into", "onto", "over", "under", "as", "is",
    "are", "was", "were", "be", "been", "being", "am", "this", "that", "these", "those", "it",
    "its", "he", "she", "they", "them", "you", "your", "we", "our", "us", "i", "me", "my", "not",
    "no", "yes", "can", "will", "would", "should", "could", "may", "might", "must", "have", "has",
    "had", "get", "got", "set", "put", "use", "used", "make", "made", "run", "ran", "new", "old",
    "add", "fix", "try", "see", "let", "all", "any", "one", "two", "up", "out", "off", "now",
    "here", "there", "when", "what", "which", "who", "how", "why", "so", "than", "too", "very",
    "just", "only", "also", "more", "most", "some", "such", "each", "about", "after", "before",
    "between", "during", "through", "until", "again", "back", "down", "out", "even", "still",
    "yet", "because", "both", "few", "other", "own", "same", "too", "only", "said",
    // Verbs and nouns the recognizer never needs told.
    "error", "warning", "info", "debug", "fatal", "true", "false", "null", "none", "void",
    "function", "return", "const", "var", "import", "export", "class", "type", "async", "await",
    "yield", "throw", "catch", "case", "break", "default", "public", "private", "file", "files",
    "line", "lines", "name", "names", "value", "values", "data", "list", "size", "count", "index",
    "text", "string", "number", "boolean", "object", "array", "map", "set", "key", "value",
    // Shell and tooling vocabulary: present on every screen, never a term.
    "cd", "ls", "rm", "cp", "mv", "cat", "echo", "grep", "sed", "awk", "git", "npm", "bun", "pnpm",
    "yarn", "cargo", "rust", "python", "node", "deno", "docker", "kubectl", "ssh", "curl", "make",
    "build", "test", "tests", "run", "start", "stop", "install", "update", "upgrade", "help",
    "version", "status", "list", "show", "info", "config", "log", "logs", "user", "users", "home",
    "root", "usr", "var", "etc", "tmp", "bin", "lib", "src", "dev", "prod", "true", "false", "yes",
    "exit", "open", "close", "read", "write", "save", "load", "send", "receive", "failed",
    "failure", "success", "ok", "done", "todo", "note", "notes",
];

/// Whether a token is one of the words keyterm biasing must not spend a term on.
#[must_use]
pub fn is_stopword(token: &str) -> bool {
    let lowered = token.to_lowercase();
    STOPWORDS.contains(&lowered.as_str())
}

#[cfg(test)]
mod tests {
    use super::{STOPWORDS, is_stopword};

    #[test]
    fn the_common_vocabulary_is_rejected() {
        for word in ["the", "error", "return", "cargo", "build", "INFO"] {
            assert!(is_stopword(word), "{word} should be a stopword");
        }
    }

    #[test]
    fn a_project_term_is_not() {
        for word in ["kysely", "coordFactory", "compose_dictation", "roost"] {
            assert!(!is_stopword(word), "{word} should be a term");
        }
    }

    #[test]
    fn the_set_has_no_capitalised_or_padded_entries() {
        for word in STOPWORDS {
            assert_eq!(*word, word.to_lowercase(), "{word} is not lowercase");
            assert_eq!(word.trim(), *word, "{word} is padded");
        }
    }
}
