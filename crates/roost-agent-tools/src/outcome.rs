//! The result of one tool call, as the worker returns it to the coordinator:
//! the model-facing text, whether the model should read it as a failure, and
//! a JSON side channel the UI may render (diff preview, diagnostics counts).

/// One finished tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutcome {
    pub is_error: bool,
    pub content: String,
    /// A JSON object; `"{}"` when the tool has nothing structured to add.
    pub details_json: String,
}

impl ToolOutcome {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            is_error: false,
            content: content.into(),
            details_json: "{}".to_owned(),
        }
    }

    pub fn failure(content: impl Into<String>) -> Self {
        Self {
            is_error: true,
            content: content.into(),
            details_json: "{}".to_owned(),
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details_json = details.to_string();
        self
    }
}
