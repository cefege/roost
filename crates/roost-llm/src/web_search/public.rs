//! OMP public search fan-out and consensus ranking port (MIT).
//! Keyless engines execute concurrently so one blocked scraper does not delay the aggregate.
//! URL normalization merges spelling variations while preserving stable engine-priority ties.

use std::{collections::HashMap, time::Duration};

use futures::{StreamExt, stream::FuturesUnordered};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::{
    ParsedQuery, SearchQuery, search_engine,
    types::{SearchResponse, SearchSource},
};
use crate::{Endpoints, LlmError};

const PUBLIC_ENGINE_IDS: [&str; 5] = ["duckduckgo", "startpage", "ecosia", "google", "mojeek"];

#[derive(Clone)]
struct RankedSource {
    source: SearchSource,
    engines: usize,
    best_rank: usize,
    order: usize,
}

pub(super) async fn search_public(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    query: &SearchQuery,
    parsed: &ParsedQuery,
    cancel: CancellationToken,
) -> Result<SearchResponse, LlmError> {
    let mut pending = FuturesUnordered::new();
    for engine in PUBLIC_ENGINE_IDS {
        let http = http.clone();
        let endpoints = endpoints.clone();
        let query = query.clone();
        let parsed = parsed.clone();
        let cancel = cancel.clone();
        pending.push(Box::pin(async move {
            let result =
                search_engine(&http, &endpoints, engine, None, &query, &parsed, cancel).await;
            (engine, result)
        }));
    }
    let mut soft_deadline = Box::pin(sleep(Duration::from_secs(5)));
    let mut hard_deadline = Box::pin(sleep(Duration::from_secs(30)));
    let mut responses = Vec::new();
    let mut failures = Vec::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            _ = &mut hard_deadline => break,
            _ = &mut soft_deadline, if !responses.is_empty() => break,
            response = pending.next() => match response {
                Some((engine, Ok(response))) => responses.push((engine, response)),
                Some((engine, Err(error))) => failures.push(format!("{engine}: {error}")),
                None => break,
            }
        }
    }
    if responses.is_empty() {
        return Err(LlmError::Network(format!(
            "All public search engines failed: {}",
            failures.join("; ")
        )));
    }
    let mut merged: HashMap<String, RankedSource> = HashMap::new();
    for (_, response) in &responses {
        for (rank, source) in response.sources.iter().enumerate() {
            let key = dedup_key(&source.url);
            match merged.get_mut(&key) {
                Some(existing) => {
                    existing.engines += 1;
                    if rank < existing.best_rank {
                        existing.best_rank = rank;
                        existing.source.title.clone_from(&source.title);
                        existing.source.url.clone_from(&source.url);
                    }
                    if source.snippet.as_ref().is_some_and(|snippet| {
                        snippet.len() > existing.source.snippet.as_ref().map_or(0, String::len)
                    }) {
                        existing.source.snippet.clone_from(&source.snippet);
                    }
                    if existing.source.published.is_none() {
                        existing.source.published = source.published.clone();
                    }
                    existing.source.age_seconds =
                        existing.source.age_seconds.or(source.age_seconds);
                }
                None => {
                    merged.insert(
                        key,
                        RankedSource {
                            source: source.clone(),
                            engines: 1,
                            best_rank: rank,
                            order: merged.len(),
                        },
                    );
                }
            }
        }
    }
    let max_results = query
        .num_search_results
        .or(query.limit)
        .unwrap_or(15)
        .clamp(1, 30) as usize;
    let mut ranked: Vec<_> = merged.into_values().collect();
    ranked.sort_by_key(|entry| {
        (
            std::cmp::Reverse(entry.engines),
            entry.best_rank,
            entry.order,
        )
    });
    let sources = ranked
        .into_iter()
        .take(max_results)
        .map(|entry| entry.source)
        .collect();
    Ok(SearchResponse {
        provider: "public".to_owned(),
        answer: None,
        sources,
        citations: Vec::new(),
        related_questions: Vec::new(),
        search_queries: Vec::new(),
    })
}

fn dedup_key(raw_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(raw_url) else {
        return raw_url.to_owned();
    };
    let host = url
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .trim_start_matches("www.")
        .to_owned();
    let path = if url.path().len() > 1 {
        url.path().trim_end_matches('/')
    } else {
        url.path()
    };
    format!("{host}{path}{}", url.query().unwrap_or_default())
}
