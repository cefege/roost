//! Coordinator-side fetch and Exa search harness tools.
//! The LLM adapter owns network access; this module validates arguments and formats bounded results.
//! Tool output is capped before it enters a model request.

use serde::Deserialize;
use serde_json::json;

use crate::runtime::AgentRuntime;
use crate::tool_round::CallResult;
use crate::toolset::{TOOL_FETCH, TOOL_WEB_SEARCH};
use crate::traits::ToolOutcome;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    5
}

pub(crate) fn fetch_spec() -> roost_llm::ToolSpec {
    roost_llm::ToolSpec {
        name: TOOL_FETCH.into(),
        description: crate::prompts::FETCH_TOOL.into(),
        parameters: json!({"type":"object","properties":{"url":{"type":"string","format":"uri"}},"required":["url"],"additionalProperties":false}),
    }
}

pub(crate) fn web_search_spec() -> roost_llm::ToolSpec {
    roost_llm::ToolSpec {
        name: TOOL_WEB_SEARCH.into(),
        description: crate::prompts::WEB_SEARCH_TOOL.into(),
        parameters: json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}),
    }
}

pub(crate) async fn run(runtime: &AgentRuntime, name: &str, args: &str) -> CallResult {
    match name {
        TOOL_FETCH => run_fetch(runtime, args).await,
        TOOL_WEB_SEARCH => run_search(runtime, args).await,
        _ => CallResult::failure(format!("Unknown web tool `{name}`.")),
    }
}

async fn run_fetch(runtime: &AgentRuntime, args: &str) -> CallResult {
    let args = match serde_json::from_str::<FetchArgs>(args) {
        Ok(args) => args,
        Err(error) => return CallResult::failure(format!("fetch requires a URL: {error}")),
    };
    match runtime.inner.llm.fetch_url(&args.url).await {
        Ok(page) => {
            let text = bounded(format!(
                "URL: {}\nContent-Type: {}\n\n{}",
                page.final_url,
                page.content_type.as_deref().unwrap_or("unknown"),
                page.text
            ));
            CallResult::plain(ToolOutcome {
                is_error: false,
                content: text,
                details_json: "{}".into(),
            })
        }
        Err(error) => CallResult::failure(error.to_string()),
    }
}

async fn run_search(runtime: &AgentRuntime, args: &str) -> CallResult {
    let args = match serde_json::from_str::<SearchArgs>(args) {
        Ok(args) => args,
        Err(error) => return CallResult::failure(format!("web_search requires a query: {error}")),
    };
    match runtime
        .inner
        .llm
        .web_search(&args.query, args.limit.clamp(1, 20))
        .await
    {
        Ok(hits) => {
            let formatted = hits
                .iter()
                .enumerate()
                .map(|(idx, hit)| {
                    format!(
                        "{}. {}\n   {}\n   {}{}",
                        idx + 1,
                        hit.title,
                        hit.url,
                        hit.snippet,
                        hit.published
                            .as_deref()
                            .map(|date| format!("\n   Published: {date}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let text = bounded(if formatted.is_empty() {
                "No results found.".into()
            } else {
                formatted
            });
            CallResult::plain(ToolOutcome {
                is_error: false,
                content: text,
                details_json: "{}".into(),
            })
        }
        Err(error) => CallResult::failure(error.to_string()),
    }
}

fn bounded(mut output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    let mut boundary = MAX_OUTPUT_BYTES.saturating_sub("\n[Output truncated at 50 KiB.]".len());
    while !output.is_char_boundary(boundary) {
        boundary -= 1;
    }
    output.truncate(boundary);
    output.push_str("\n[Output truncated at 50 KiB.]");
    output
}
