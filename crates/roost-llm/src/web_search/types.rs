//! Search request and normalized response types.
//! Engine clients produce the same shape regardless of their wire protocol.
//! `format_for_model` is the single model-facing representation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Recency {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub recency: Option<Recency>,
    pub limit: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub num_search_results: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSource {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published: Option<String>,
    pub age_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub title: String,
    pub url: String,
    pub cited_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub provider: String,
    pub answer: Option<String>,
    pub sources: Vec<SearchSource>,
    pub citations: Vec<Citation>,
    pub related_questions: Vec<String>,
    pub search_queries: Vec<String>,
}

impl SearchResponse {
    pub(super) fn empty(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            answer: None,
            sources: Vec::new(),
            citations: Vec::new(),
            related_questions: Vec::new(),
            search_queries: Vec::new(),
        }
    }
}

pub fn format_for_model(response: &SearchResponse, notes: &[String]) -> String {
    let mut sections = Vec::new();
    sections.extend(notes.iter().map(|note| format!("Note: {note}")));
    if let Some(answer) = response
        .answer
        .as_deref()
        .filter(|answer| !answer.is_empty())
    {
        sections.push(answer.to_owned());
        if !response.sources.is_empty() {
            sections.push(format!(
                "\n## Sources\n{} source(s)",
                response.sources.len()
            ));
        }
    }
    for (index, source) in response.sources.iter().enumerate() {
        let age = source
            .age_seconds
            .map(format_age)
            .or_else(|| source.published.clone());
        let age_text = age.map(|age| format!(" ({age})")).unwrap_or_default();
        let mut rendered = format!(
            "[{}] {}{}\n    {}",
            index + 1,
            source.title,
            age_text,
            source.url
        );
        if let Some(snippet) = source
            .snippet
            .as_deref()
            .filter(|snippet| !snippet.is_empty())
        {
            rendered.push_str("\n    ");
            rendered.push_str(&truncate(snippet, 240));
        }
        sections.push(rendered);
    }
    if !response.citations.is_empty() {
        sections.push(format!(
            "\n## Citations\n{} citation(s)",
            response.citations.len()
        ));
        for (index, citation) in response.citations.iter().enumerate() {
            let title = if citation.title.is_empty() {
                &citation.url
            } else {
                &citation.title
            };
            let mut rendered = format!("[{}] {title}\n    {}", index + 1, citation.url);
            if let Some(text) = citation.cited_text.as_deref() {
                rendered.push_str("\n    ");
                rendered.push_str(&truncate(text, 240));
            }
            sections.push(rendered);
        }
    }
    if !response.related_questions.is_empty() {
        sections.push(format!(
            "\n## Related\n{} question(s)",
            response.related_questions.len()
        ));
        sections.extend(
            response
                .related_questions
                .iter()
                .map(|question| format!("- {question}")),
        );
    }
    if !response.search_queries.is_empty() {
        sections.push(format!("Search queries: {}", response.search_queries.len()));
        sections.extend(
            response
                .search_queries
                .iter()
                .take(3)
                .map(|query| format!("- {}", truncate(query, 120))),
        );
    }
    sections.join("\n")
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(limit.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn format_age(seconds: u64) -> String {
    if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}
