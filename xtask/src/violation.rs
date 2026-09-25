//! One lint finding, rendered identically for every check so a CI log line
//! always carries the same four things: where, what, which rule, and which
//! memory document explains why the rule exists.

/// A single gate failure. `line` is 1-based; `0` means "whole file".
#[derive(Debug, Clone)]
pub struct Violation {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub rule: String,
    pub memory: String,
}

impl Violation {
    pub fn new(
        file: impl Into<String>,
        line: usize,
        text: impl Into<String>,
        rule: impl Into<String>,
        memory: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            line,
            text: text.into(),
            rule: rule.into(),
            memory: memory.into(),
        }
    }

    /// The report a developer reads: location, the defect, and the rule.
    pub fn render(&self) -> String {
        format!(
            "{}:{}: {}\n    rule:   {}\n    memory: {}",
            self.file, self.line, self.text, self.rule, self.memory
        )
    }
}
