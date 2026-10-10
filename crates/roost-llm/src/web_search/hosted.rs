//! Ported from oh-my-pi packages/coding-agent/src/web/search/providers/{anthropic,codex,openrouter}.ts (MIT).
//! Uses the conversation provider's existing credentials and identity headers for its hosted search tool.
//! A host that returns no citations is treated as having silently ignored the requested tool.

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    ParsedQuery, SearchQuery,
    types::{Citation, SearchResponse, SearchSource},
};
use crate::{Endpoints, LlmError, ModelInfo, ResolvedAuth, WireApi};

pub async fn hosted_search(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    model: &ModelInfo,
    auth: ResolvedAuth,
    query: &SearchQuery,
    parsed: &ParsedQuery,
    cancel: CancellationToken,
) -> Result<SearchResponse, LlmError> {
    let (provider, url, body) = match model.api {
        WireApi::AnthropicMessages
            if model.provider == "anthropic" || model.provider == "openrouter" =>
        {
            let base = endpoints.base(&model.provider, &model.base_url);
            let mut headers = anthropic_headers(&auth, &model.provider)?;
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            if auth.is_oauth() {
                headers.insert(
                    "anthropic-beta",
                    HeaderValue::from_static("claude-code-20250219,oauth-2025-04-20"),
                );
                headers.insert("user-agent", HeaderValue::from_static("claude-cli/2.1.280"));
                headers.insert("x-app", HeaderValue::from_static("cli"));
            }
            let request_body = json!({"model":model.id,"max_tokens":query.max_tokens.unwrap_or(4096),"messages":[{"role":"user","content":format_query(query, parsed)}],"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":5}],"stream":false});
            (
                "anthropic",
                format!("{}/v1/messages", base.trim_end_matches('/')),
                (headers, request_body),
            )
        }
        WireApi::OpenAiCodexResponses => {
            let base = endpoints.base(&model.provider, &model.base_url);
            let mut headers = bearer_headers(&auth)?;
            if let ResolvedAuth::OAuth {
                account_id: Some(account_id),
                ..
            } = &auth
            {
                headers.insert(
                    "chatgpt-account-id",
                    HeaderValue::from_str(account_id)
                        .map_err(|error| LlmError::Auth(error.to_string()))?,
                );
            }
            headers.insert(
                "openai-beta",
                HeaderValue::from_static("responses=experimental"),
            );
            let request_body = json!({"model":model.id,"input":format_query(query, parsed),"tools":[{"type":"web_search"}],"tool_choice":{"type":"web_search"},"include":["web_search_call.action.sources"]});
            (
                "codex",
                format!("{}/codex/responses", base.trim_end_matches('/')),
                (headers, request_body),
            )
        }
        WireApi::OpenAiCompletions if model.provider == "openrouter" => {
            let base = endpoints.base(&model.provider, &model.base_url);
            let headers = bearer_headers(&auth)?;
            let request_body = json!({"model":model.id,"messages":[{"role":"user","content":format_query(query, parsed)}],"plugins":[{"id":"web"}],"max_tokens":query.max_tokens,"temperature":query.temperature});
            (
                "openrouter",
                format!("{}/chat/completions", base.trim_end_matches('/')),
                (headers, request_body),
            )
        }
        _ => {
            return Err(LlmError::Http {
                status: 501,
                body: "no hosted web search for this model provider".to_owned(),
            });
        }
    };
    let (headers, body) = body;
    let response = tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), response = http.post(url).headers(headers).json(&body).send() => response? };
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(LlmError::Http {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]).into_owned(),
        });
    }
    let payload = parse_response(&bytes)?;
    let mut result = match provider {
        "anthropic" => parse_anthropic(&payload),
        "codex" => parse_codex(&payload),
        _ => parse_openrouter(&payload),
    };
    result.provider = provider.to_owned();
    if result.sources.is_empty() && result.citations.is_empty() {
        return Err(LlmError::Decode(
            "hosted web search returned an answer without sources".to_owned(),
        ));
    }
    Ok(result)
}

fn format_query(query: &SearchQuery, parsed: &ParsedQuery) -> String {
    if parsed.sites.is_empty() {
        query.query.clone()
    } else {
        format!(
            "{} {}",
            query.query,
            parsed
                .sites
                .iter()
                .map(|site| format!("site:{site}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}
fn header(value: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value).map_err(|error| LlmError::Auth(error.to_string()))
}
fn bearer_headers(auth: &ResolvedAuth) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        header(&format!("Bearer {}", auth.secret()))?,
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "accept",
        HeaderValue::from_static("text/event-stream, application/json"),
    );
    Ok(headers)
}
fn anthropic_headers(auth: &ResolvedAuth, provider: &str) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    if auth.is_oauth() || provider == "openrouter" {
        headers.insert(
            "authorization",
            header(&format!("Bearer {}", auth.secret()))?,
        );
    } else {
        headers.insert("x-api-key", header(auth.secret())?);
    }
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    Ok(headers)
}
fn parse_response(bytes: &[u8]) -> Result<Value, LlmError> {
    if let Ok(value) = serde_json::from_slice(bytes) {
        return Ok(value);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut events = Vec::new();
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ")
            && let Ok(value) = serde_json::from_str::<Value>(data)
        {
            events.push(value);
        }
    }
    if events.is_empty() {
        return Err(LlmError::Decode(
            "hosted search returned invalid JSON/SSE".to_owned(),
        ));
    }
    Ok(Value::Array(events))
}
fn parse_anthropic(payload: &Value) -> SearchResponse {
    let mut result = SearchResponse::empty("anthropic");
    let blocks: &[Value] = payload.as_array().map_or_else(
        || {
            payload
                .get("content")
                .and_then(Value::as_array)
                .map_or(&[], Vec::as_slice)
        },
        Vec::as_slice,
    );
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    result.answer.get_or_insert_with(String::new).push_str(text);
                }
            }
            "web_search_tool_result" => {
                if let Some(items) = block.get("content").and_then(Value::as_array) {
                    for item in items {
                        add_source(&mut result, item);
                    }
                }
            }
            "content_block_delta" => {
                if let Some(text) = block.pointer("/delta/text").and_then(Value::as_str) {
                    result.answer.get_or_insert_with(String::new).push_str(text);
                }
            }
            "message_start" | "content_block_start" => {
                if let Some(content) = block.pointer("/content_block")
                    && content.get("type").and_then(Value::as_str) == Some("web_search_tool_result")
                    && let Some(items) = content.get("content").and_then(Value::as_array)
                {
                    for item in items {
                        add_source(&mut result, item);
                    }
                }
            }
            _ => {}
        }
    }
    result
}
fn parse_codex(payload: &Value) -> SearchResponse {
    let mut result = SearchResponse::empty("codex");
    let events = payload
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![payload.clone()]);
    for event in events {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(text) = event.get("delta").and_then(Value::as_str) {
                    result.answer.get_or_insert_with(String::new).push_str(text);
                }
            }
            "response.output_item.done" | "web_search_call" => {
                let item = event.get("item").unwrap_or(&event);
                if let Some(sources) = item
                    .pointer("/action/sources")
                    .or_else(|| item.get("sources"))
                    .and_then(Value::as_array)
                {
                    for source in sources {
                        add_source(&mut result, source);
                    }
                }
            }
            _ => {}
        }
    }
    result
}
fn parse_openrouter(payload: &Value) -> SearchResponse {
    let mut result = SearchResponse::empty("openrouter");
    result.answer = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(annotations) = payload
        .pointer("/choices/0/message/annotations")
        .and_then(Value::as_array)
    {
        for annotation in annotations {
            let source = annotation.get("url_citation").unwrap_or(annotation);
            add_source(&mut result, source);
        }
    }
    result
}
fn add_source(result: &mut SearchResponse, value: &Value) {
    let url = value
        .get("url")
        .or_else(|| value.get("link"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if url.is_empty() {
        return;
    }
    let title = value
        .get("title")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(url)
        .to_owned();
    let text = value
        .get("cited_text")
        .or_else(|| value.get("snippet"))
        .or_else(|| value.get("text"))
        .or_else(|| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    result.sources.push(SearchSource {
        title: title.clone(),
        url: url.to_owned(),
        snippet: text.clone(),
        published: None,
        age_seconds: None,
    });
    result.citations.push(Citation {
        title,
        url: url.to_owned(),
        cited_text: text,
    });
}
