//! Values returned by the coordinator-side web tools.
//! The LLM adapter owns network policy and provider access.
//! These records keep transport details out of tool execution.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPage {
    pub final_url: String,
    pub content_type: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published: Option<String>,
}
