//! Ported from oh-my-pi packages/coding-agent/src/web/search/providers/*.ts (MIT).
//! Implements keyed APIs, SearXNG JSON, and browser-header HTML search engines.
//! The normalized response boundary keeps provider wire formats out of roost-agent.

use super::{
    response::normalize,
    scraper::{browser_headers, search_html},
};
use reqwest::{Method, header::HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    ParsedQuery, SearchQuery,
    types::{Recency, SearchResponse},
};
use crate::{Endpoints, LlmError};

pub const ENGINE_IDS: &[&str] = &[
    "exa",
    "parallel",
    "firecrawl",
    "brave",
    "tavily",
    "perplexity",
    "jina",
    "kagi",
    "searxng",
    "duckduckgo",
    "startpage",
    "google",
    "ecosia",
    "mojeek",
    "public",
];

pub fn engine_needs_key(id: &str) -> bool {
    matches!(
        id,
        "exa" | "parallel" | "firecrawl" | "brave" | "tavily" | "perplexity" | "jina" | "kagi"
    )
}
pub fn engine_label(id: &str) -> &'static str {
    match id {
        "exa" => "Exa",
        "parallel" => "Parallel",
        "firecrawl" => "Firecrawl",
        "brave" => "Brave",
        "tavily" => "Tavily",
        "perplexity" => "Perplexity",
        "jina" => "Jina",
        "kagi" => "Kagi",
        "searxng" => "SearXNG",
        "duckduckgo" => "DuckDuckGo",
        "startpage" => "Startpage",
        "google" => "Google",
        "ecosia" => "Ecosia",
        "mojeek" => "Mojeek",
        "public" => "Public",
        _ => "Unknown",
    }
}

pub async fn search_engine(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    engine_id: &str,
    credential: Option<&str>,
    query: &SearchQuery,
    parsed: &ParsedQuery,
    cancel: CancellationToken,
) -> Result<SearchResponse, LlmError> {
    if cancel.is_cancelled() {
        return Err(LlmError::Cancelled);
    }
    if !ENGINE_IDS.contains(&engine_id) {
        return Err(LlmError::Decode(format!(
            "unknown web search engine {engine_id}"
        )));
    }
    if engine_needs_key(engine_id) && credential.is_none() {
        return Err(LlmError::NoCredential {
            provider: engine_id.to_owned(),
        });
    }
    if engine_id == "public" {
        return super::public::search_public(http, endpoints, query, parsed, cancel).await;
    }
    let base = default_base(engine_id, credential);
    let base = endpoints.base(engine_id, &base);
    let mut headers = browser_headers();
    let count = query
        .num_search_results
        .or(query.limit)
        .unwrap_or(10)
        .clamp(1, 20);
    let query_text = native_query(query, parsed);
    let (method, url, body) = match engine_id {
        "exa" => {
            headers.insert("x-api-key", header(credential.unwrap_or_default())?);
            (
                Method::POST,
                join(&base, "/search"),
                Some(
                    json!({"query":query_text,"type":"auto","numResults":count,"contents":{"text":{"maxCharacters":500},"highlights":true},"includeDomains":parsed.sites}),
                ),
            )
        }
        "parallel" => {
            headers.insert("x-api-key", header(credential.unwrap_or_default())?);
            headers.insert(
                "parallel-beta",
                HeaderValue::from_static("search-extract-2025-10-10"),
            );
            let mut body = json!({"objective":query_text,"search_queries":[query_text],"mode":"fast","excerpts":{"max_chars_per_result":10000}});
            if let Some(policy) = source_policy(parsed, query.recency) {
                body["source_policy"] = policy;
            }
            (Method::POST, join(&base, "/v1beta/search"), Some(body))
        }
        "firecrawl" => {
            headers.insert(
                "authorization",
                header(&format!("Bearer {}", credential.unwrap_or_default()))?,
            );
            (
                Method::POST,
                join(&base, "/search"),
                Some(json!({"query":query_text,"limit":count,"sources":[{"type":"web"}]})),
            )
        }
        "brave" => {
            headers.insert(
                "x-subscription-token",
                header(credential.unwrap_or_default())?,
            );
            let mut url = reqwest::Url::parse(&join(&base, "/res/v1/web/search"))
                .map_err(|e| LlmError::Decode(e.to_string()))?;
            url.query_pairs_mut()
                .append_pair("q", &query_text)
                .append_pair("count", &count.to_string())
                .append_pair("extra_snippets", "true");
            if let Some(freshness) = recency_param(query.recency) {
                url.query_pairs_mut().append_pair("freshness", freshness);
            }
            (Method::GET, url.to_string(), None)
        }
        "tavily" => {
            headers.insert(
                "authorization",
                header(&format!("Bearer {}", credential.unwrap_or_default()))?,
            );
            let mut body = json!({
                "query": query_text,
                "search_depth": "basic",
                "max_results": count.min(20),
                "include_answer": "advanced",
                "include_raw_content": false
            });
            if !parsed.sites.is_empty() {
                body["include_domains"] = json!(parsed.sites);
            }
            if !parsed.excluded_sites.is_empty() {
                body["exclude_domains"] = json!(parsed.excluded_sites);
            }
            if let Some(after) = &parsed.after {
                body["start_date"] = json!(after);
            }
            if let Some(before) = &parsed.before {
                body["end_date"] = json!(before);
            }
            if parsed.after.is_none()
                && parsed.before.is_none()
                && let Some(period) = query.recency
            {
                body["time_range"] = json!(match period {
                    Recency::Day => "day",
                    Recency::Week => "week",
                    Recency::Month => "month",
                    Recency::Year => "year",
                });
            }
            (Method::POST, join(&base, "/search"), Some(body))
        }
        "perplexity" => {
            headers.insert(
                "authorization",
                header(&format!("Bearer {}", credential.unwrap_or_default()))?,
            );
            (
                Method::POST,
                join(&base, "/chat/completions"),
                Some(
                    json!({"model":"sonar","messages":[{"role":"user","content":query_text}],"max_tokens":query.max_tokens,"temperature":query.temperature,"return_related_questions":true}),
                ),
            )
        }
        "jina" => {
            headers.insert(
                "authorization",
                header(&format!("Bearer {}", credential.unwrap_or_default()))?,
            );
            headers.insert("x-respond-with", HeaderValue::from_static("no-content"));
            headers.insert("x-retain-images", HeaderValue::from_static("none"));
            let mut url =
                reqwest::Url::parse(&base).map_err(|error| LlmError::Decode(error.to_string()))?;
            url.path_segments_mut()
                .map_err(|_| {
                    LlmError::Decode("Jina search endpoint cannot be a base URL".to_owned())
                })?
                .push(&query_text);
            url.query_pairs_mut()
                .append_pair("count", &count.to_string());
            if let [site] = parsed.sites.as_slice() {
                headers.insert("x-site", header(site)?);
            }
            (Method::GET, url.to_string(), None)
        }
        "kagi" => {
            headers.insert(
                "authorization",
                header(&format!("Bearer {}", credential.unwrap_or_default()))?,
            );
            (
                Method::POST,
                join(&base, "/api/v1/search"),
                Some(json!({
                    "query": query_text,
                    "limit": count.min(40),
                    "filters": {"after": parsed.after, "before": parsed.before}
                })),
            )
        }
        "searxng" => {
            let mut url = reqwest::Url::parse(&join(&base, "/search"))
                .map_err(|e| LlmError::Decode(e.to_string()))?;
            url.query_pairs_mut()
                .append_pair("q", &query_text)
                .append_pair("format", "json")
                .append_pair("categories", "general");
            let time_range = match query.recency {
                Some(Recency::Day) => Some("day"),
                Some(Recency::Week | Recency::Month) => Some("month"),
                Some(Recency::Year) => Some("year"),
                None => None,
            };
            if let Some(time) = time_range {
                url.query_pairs_mut().append_pair("time_range", time);
            }
            (Method::GET, url.to_string(), None)
        }
        _ => {
            return search_html(
                http,
                super::scraper::HtmlSearchRequest {
                    base: &base,
                    engine: engine_id,
                    query: &query_text,
                    count,
                    recency: query.recency,
                    headers: &headers,
                },
                cancel,
            )
            .await;
        }
    };
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let mut request = http.request(method, url).headers(headers);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), result = request.send() => result? };
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        return Err(LlmError::Http {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]).into_owned(),
        });
    }
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|e| LlmError::Decode(e.to_string()))?;
    Ok(normalize(engine_id, payload))
}

fn default_base(id: &str, credential: Option<&str>) -> String {
    match id {
        "exa" => "https://api.exa.ai",
        "parallel" => "https://api.parallel.ai",
        "firecrawl" => "https://api.firecrawl.dev/v2",
        "brave" => "https://api.search.brave.com",
        "tavily" => "https://api.tavily.com",
        "perplexity" => "https://api.perplexity.ai",
        "jina" => "https://s.jina.ai",
        "kagi" => "https://kagi.com",
        "searxng" => credential.unwrap_or("https://searx.be"),
        "duckduckgo" => "https://html.duckduckgo.com",
        "startpage" => "https://www.startpage.com",
        "ecosia" => "https://www.ecosia.org",
        "mojeek" => "https://www.mojeek.de",
        _ => "",
    }
    .to_owned()
}
fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}
fn header(value: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value).map_err(|error| LlmError::Decode(error.to_string()))
}
fn native_query(query: &SearchQuery, parsed: &ParsedQuery) -> String {
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
fn recency_param(recency: Option<Recency>) -> Option<&'static str> {
    match recency {
        Some(Recency::Day) => Some("pd"),
        Some(Recency::Week) => Some("pw"),
        Some(Recency::Month) => Some("pm"),
        Some(Recency::Year) => Some("py"),
        None => None,
    }
}
fn source_policy(parsed: &ParsedQuery, _recency: Option<Recency>) -> Option<Value> {
    let include: Vec<_> = parsed
        .sites
        .iter()
        .map(|site| site.split('/').next().unwrap_or(site))
        .collect();
    let exclude: Vec<_> = parsed
        .excluded_sites
        .iter()
        .map(|site| site.split('/').next().unwrap_or(site))
        .collect();
    if include.is_empty() && exclude.is_empty() && parsed.after.is_none() {
        return None;
    }
    let mut policy = json!({});
    if !include.is_empty() {
        policy["include_domains"] = json!(include);
    }
    if include.is_empty() && !exclude.is_empty() {
        policy["exclude_domains"] = json!(exclude);
    }
    if let Some(after) = &parsed.after {
        policy["after_date"] = json!(after);
    }
    Some(policy)
}
