//! Provider response adapters produce the shared source, citation and answer types.
//! OMP response fields vary substantially, so normalization is kept apart from transport.
//! Unknown and missing source fields are ignored rather than trusted as valid URLs.

use serde_json::Value;

use super::types::{Citation, SearchResponse, SearchSource};

pub(super) fn normalize(provider: &str, payload: Value) -> SearchResponse {
    let mut result = SearchResponse::empty(provider);
    result.answer = payload
        .get("answer")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            payload
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    result.related_questions = payload
        .get("related_questions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    result.search_queries = payload
        .get("search_queries")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(citations) = payload.get("citations").and_then(Value::as_array) {
        for url in citations.iter().filter_map(Value::as_str) {
            result.citations.push(Citation {
                title: url.to_owned(),
                url: url.to_owned(),
                cited_text: None,
            });
        }
    }
    let nested = payload
        .pointer("/data/search")
        .and_then(Value::as_array)
        .or_else(|| payload.pointer("/data/web").and_then(Value::as_array));
    let candidates = ["results", "web", "data", "organic", "items"];
    let values = nested
        .or_else(|| {
            candidates
                .iter()
                .find_map(|key| payload.get(*key).and_then(Value::as_array))
        })
        .or_else(|| payload.pointer("/web/results").and_then(Value::as_array));
    if let Some(values) = values {
        for item in values {
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.is_empty() {
                continue;
            }
            let title = item
                .get("title")
                .or_else(|| item.get("name"))
                .or_else(|| item.get("t"))
                .and_then(Value::as_str)
                .unwrap_or(url);
            let snippet = item
                .get("text")
                .or_else(|| item.get("snippet"))
                .or_else(|| item.get("description"))
                .or_else(|| item.get("excerpt"))
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            result.sources.push(SearchSource {
                title: title.to_owned(),
                url: url.to_owned(),
                snippet,
                published: item
                    .get("publishedDate")
                    .or_else(|| item.get("published"))
                    .or_else(|| item.get("time"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                age_seconds: None,
            });
        }
    }
    result
}
