//! Ported from oh-my-pi packages/coding-agent/src/web/search/index.ts (MIT).
//! The LLM adapter owns provider credentials and network requests.
//! Tool output is capped before it enters a model request.

use serde::Deserialize;
use serde_json::json;

use crate::runtime::AgentRuntime;
use crate::tool_round::{CallResult, CallScope};
use crate::toolset::{TOOL_FETCH, TOOL_WEB_SEARCH};
use crate::traits::ToolOutcome;
use roost_llm::web_search::{
    Recency, SearchQuery, SearchResponse, apply_constraints, format_for_model, parse_query,
};

const MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    recency: Option<Recency>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    num_search_results: Option<u32>,
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
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "recency": {"type": "string", "enum": ["day", "week", "month", "year"]},
                "limit": {"type": "number"},
                "max_tokens": {"type": "number"},
                "temperature": {"type": "number"},
                "num_search_results": {"type": "number"}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

pub(crate) async fn run(scope: &CallScope<'_>, name: &str, args: &str) -> CallResult {
    match name {
        TOOL_FETCH => run_fetch(scope.runtime, args).await,
        TOOL_WEB_SEARCH => run_search(scope, args).await,
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

async fn run_search(scope: &CallScope<'_>, input: &str) -> CallResult {
    let args = match serde_json::from_str::<SearchArgs>(input) {
        Ok(args) => args,
        Err(error) => return CallResult::failure(format!("web_search requires a query: {error}")),
    };
    let query = SearchQuery {
        query: args.query,
        recency: args.recency,
        limit: args.limit,
        max_tokens: args.max_tokens,
        temperature: args.temperature,
        num_search_results: args.num_search_results,
    };
    let parsed = parse_query(&query.query);
    let settings = match scope.runtime.inner.store.settings().await {
        Ok(settings) => settings,
        Err(error) => return CallResult::failure(error.to_string()),
    };
    let selector = settings
        .model_roles
        .get(&crate::Role::Web)
        .map(String::as_str)
        .unwrap_or(crate::roles::DEFAULT_WEB_CHAIN);
    let candidates = match crate::roles::parse_selector(selector) {
        Ok(candidates) => candidates,
        Err(error) => return CallResult::failure(format!("Invalid web search chain: {error}")),
    };

    let mut failures = Vec::new();
    let mut hosted_model = None;
    for candidate in candidates {
        let (provider_id, result) = match candidate {
            crate::roles::SelectorCandidate::WebEngine(engine_id) => {
                let base_url = if engine_id == "searxng" {
                    settings.searxng_url.as_deref()
                } else {
                    None
                };
                if engine_id == "searxng" && base_url.is_none() {
                    continue;
                }
                if engine_id != "searxng"
                    && !scope.runtime.inner.llm.engine_configured(&engine_id).await
                {
                    continue;
                }
                let result = scope
                    .runtime
                    .inner
                    .llm
                    .search_engine(&engine_id, base_url, &query, &parsed, scope.cancel.clone())
                    .await;
                (engine_id, result)
            }
            crate::roles::SelectorCandidate::WebHosted => {
                let model =
                    match &hosted_model {
                        Some(model) => model,
                        None => {
                            hosted_model =
                                match crate::turn::conversation_model(scope.runtime, scope.record)
                                    .await
                                {
                                    Ok(model) => Some(model.info),
                                    Err(error) => {
                                        failures.push(format!("hosted: {error}"));
                                        continue;
                                    }
                                };
                            match &hosted_model {
                                Some(model) => model,
                                None => continue,
                            }
                        }
                    };
                let result = scope
                    .runtime
                    .inner
                    .llm
                    .hosted_search(
                        model,
                        &scope.record.id,
                        &query,
                        &parsed,
                        scope.cancel.clone(),
                    )
                    .await;
                ("hosted".to_owned(), result)
            }
            crate::roles::SelectorCandidate::Model { .. }
            | crate::roles::SelectorCandidate::Role(_) => continue,
        };
        match result {
            Ok(response) => {
                let label = provider_label(&provider_id);
                if provider_id == "hosted"
                    && response.sources.is_empty()
                    && response.citations.is_empty()
                {
                    failures.push(format!("{label}: returned no sources or citations"));
                    continue;
                }
                let formatted = format_response(response, &parsed);
                if formatted.outcome.is_error {
                    failures.push(format!("{label}: {}", formatted.outcome.content));
                    continue;
                }
                return formatted;
            }
            Err(error) if matches!(error, roost_llm::LlmError::Cancelled) => {
                return CallResult::failure(error.to_string());
            }
            Err(error) => failures.push(format!("{}: {error}", provider_label(&provider_id))),
        }
    }
    let message = match failures.as_slice() {
        [] => "No configured web search providers are available.".to_owned(),
        [failure] => failure.clone(),
        _ => format!("All web search providers failed: {}", failures.join("; ")),
    };
    CallResult::failure(message)
}
fn provider_label(provider_id: &str) -> &'static str {
    if provider_id == "hosted" {
        "hosted"
    } else {
        roost_llm::web_search::engine_label(provider_id)
    }
}

fn format_response(
    mut response: SearchResponse,
    parsed: &roost_llm::web_search::ParsedQuery,
) -> CallResult {
    let (sources, notes) = apply_constraints(response.sources, parsed);
    response.sources = sources;
    if response.answer.is_none()
        && response.sources.is_empty()
        && response.citations.is_empty()
        && response.related_questions.is_empty()
        && response.search_queries.is_empty()
    {
        return CallResult::failure(format!(
            "{} returned no renderable search content.",
            response.provider
        ));
    }
    CallResult::plain(ToolOutcome {
        is_error: false,
        content: bounded(format_for_model(&response, &notes)),
        details_json: "{}".into(),
    })
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
