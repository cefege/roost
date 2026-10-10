//! Values returned by the coordinator-side URL fetch tool.
//! Search results use the normalized records owned by roost-llm.
//! Fetch transport details stay out of tool execution.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPage {
    pub final_url: String,
    pub content_type: Option<String>,
    pub text: String,
}
