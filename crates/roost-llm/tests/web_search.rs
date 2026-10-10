use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use roost_llm::{
    Endpoints,
    web_search::{
        Recency, SearchQuery, SearchSource, apply_constraints, parse_query, search_engine,
    },
};
use serde_json::Value;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_util::sync::CancellationToken;

#[path = "web_search/hosted.rs"]
mod hosted;
#[path = "web_search/scrapers.rs"]
mod scrapers;

#[derive(Clone, Debug)]
struct RecordedRequest {
    uri: String,
    headers: HeaderMap,
    body: String,
}

#[derive(Clone)]
struct Fixture {
    body: &'static str,
    home_body: Option<&'static str>,
    content_type: &'static str,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

async fn serve_fixture(
    State(state): State<Fixture>,
    request: axum::extract::Request,
) -> Response<Body> {
    let uri = request.uri().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap();
    state.requests.lock().await.push(RecordedRequest {
        uri: uri.clone(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let response_body = if uri == "/" {
        state.home_body.unwrap_or(state.body)
    } else {
        state.body
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", state.content_type)
        .body(Body::from(response_body))
        .unwrap()
}

async fn server(
    body: &'static str,
    content_type: &'static str,
) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
    server_with_home(body, content_type, None).await
}

async fn server_with_home(
    body: &'static str,
    content_type: &'static str,
    home_body: Option<&'static str>,
) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", any(serve_fixture))
        .route("/{*path}", any(serve_fixture))
        .with_state(Fixture {
            body,
            home_body,
            content_type,
            requests: requests.clone(),
        });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), requests)
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        query: text.to_owned(),
        recency: Some(Recency::Week),
        limit: Some(5),
        max_tokens: None,
        temperature: None,
        num_search_results: Some(5),
    }
}
fn endpoints(engine: &str, base: String) -> Endpoints {
    Endpoints::with_overrides(BTreeMap::from([(engine.to_owned(), base)]))
}

#[test]
fn query_parser_recognizes_terms_phrases_and_directives() {
    let parsed = parse_query(
        "\"rust async\" site:docs.rs -deprecated intitle:guide inurl:api filetype:pdf after:2025-01-02",
    );
    assert_eq!(parsed.phrases, ["rust async"]);
    assert_eq!(parsed.sites, ["docs.rs"]);
    assert_eq!(parsed.excluded_terms, ["deprecated"]);
    assert_eq!(parsed.in_titles, ["guide"]);
    assert_eq!(parsed.in_urls, ["api"]);
    assert_eq!(parsed.filetypes, ["pdf"]);
    assert_eq!(parsed.after.as_deref(), Some("2025-01-02"));
}

#[test]
fn constraints_relax_only_a_dimension_that_would_remove_every_result() {
    let sources = vec![
        SearchSource {
            title: "Rust guide".into(),
            url: "https://docs.rs/rust.pdf".into(),
            snippet: None,
            published: Some("2025-03-01T00:00:00Z".into()),
            age_seconds: None,
        },
        SearchSource {
            title: "Other guide".into(),
            url: "https://example.com/guide".into(),
            snippet: None,
            published: None,
            age_seconds: None,
        },
    ];
    let (filtered, notes) = apply_constraints(
        sources,
        &parse_query("site:missing.example filetype:pdf after:2025-01-01"),
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].title, "Rust guide");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("site:missing.example"));
}

#[tokio::test]
async fn keyed_and_json_engines_replay_fixtures_and_emit_expected_requests() {
    let engines = [
        (
            "exa",
            include_str!("fixtures/exa-search.json"),
            "x-api-key",
            "/search",
        ),
        (
            "parallel",
            include_str!("fixtures/parallel-search.json"),
            "x-api-key",
            "/v1beta/search",
        ),
        (
            "firecrawl",
            include_str!("fixtures/firecrawl-search.json"),
            "authorization",
            "/search",
        ),
        (
            "brave",
            include_str!("fixtures/brave-search.json"),
            "x-subscription-token",
            "/res/v1/web/search",
        ),
        (
            "tavily",
            include_str!("fixtures/tavily-search.json"),
            "authorization",
            "/search",
        ),
        (
            "perplexity",
            include_str!("fixtures/perplexity-search.json"),
            "authorization",
            "/chat/completions",
        ),
        (
            "jina",
            include_str!("fixtures/jina-search.json"),
            "authorization",
            "/fixture%20query",
        ),
        (
            "kagi",
            include_str!("fixtures/kagi-search.json"),
            "authorization",
            "/api/v1/search",
        ),
        (
            "searxng",
            include_str!("fixtures/searxng-search.json"),
            "accept",
            "/search",
        ),
    ];
    for (engine, response, auth_header, expected_path) in engines {
        let (base, requests) = server(response, "application/json").await;
        let credential = if engine == "searxng" {
            Some(base.as_str())
        } else {
            Some("fixture-key")
        };
        let result = search_engine(
            &reqwest::Client::new(),
            &endpoints(engine, base.clone()),
            engine,
            credential,
            &query("fixture query"),
            &parse_query("fixture query"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.provider, engine);
        let records = requests.lock().await;
        let request = records.last().unwrap();
        assert!(
            request.uri.starts_with(expected_path),
            "{engine} request URI {}",
            request.uri
        );
        if engine != "searxng" {
            let expected_auth = if engine == "exa" || engine == "parallel" || engine == "brave" {
                "fixture-key".to_owned()
            } else {
                "Bearer fixture-key".to_owned()
            };
            assert_eq!(
                request
                    .headers
                    .get(auth_header)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_auth.as_str()),
                "{engine} auth"
            );
        }
        if matches!(
            engine,
            "exa" | "parallel" | "firecrawl" | "tavily" | "perplexity" | "kagi"
        ) {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            if engine == "exa" {
                assert_eq!(body["query"], "fixture query");
            }
            if engine == "parallel" {
                assert_eq!(body["objective"], "fixture query");
                assert_eq!(
                    request.headers.get("parallel-beta").unwrap(),
                    "search-extract-2025-10-10"
                );
            }
            if engine == "firecrawl" {
                assert_eq!(body["sources"][0]["type"], "web");
            }
            if engine == "tavily" {
                assert_eq!(body["search_depth"], "basic");
                assert_eq!(body["time_range"], "week");
            }
            if engine == "perplexity" {
                assert_eq!(body["model"], "sonar");
            }
            if engine == "kagi" {
                assert_eq!(body["query"], "fixture query");
            }
        }
        if engine == "brave" {
            assert!(
                request.uri.contains("q=fixture%20query")
                    || request.uri.contains("q=fixture+query")
            );
            assert!(request.uri.contains("freshness=pw"));
        }
        if engine == "jina" {
            assert!(request.uri.contains("count=5"));
            assert_eq!(request.headers.get("x-respond-with").unwrap(), "no-content");
        }
        if engine == "searxng" {
            assert!(request.uri.contains("format=json"));
            assert!(request.uri.contains("time_range=month"));
        }
    }
}
