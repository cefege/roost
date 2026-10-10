use super::*;
use std::collections::BTreeMap;

#[tokio::test]
async fn html_engines_replay_results_and_startpage_performs_token_handshake() {
    let cases = [
        (
            "duckduckgo",
            include_str!("../fixtures/duckduckgo-search.html"),
            "tokio - Rust - Docs.rs",
        ),
        (
            "google",
            include_str!("../fixtures/google-search.html"),
            "Example Google",
        ),
        (
            "ecosia",
            include_str!("../fixtures/ecosia-search.html"),
            "Example Ecosia",
        ),
        (
            "mojeek",
            include_str!("../fixtures/mojeek-search.html"),
            "Example Mojeek",
        ),
    ];
    for (engine, fixture, title) in cases {
        let (base, requests) = server(fixture, "text/html").await;
        let result = search_engine(
            &reqwest::Client::new(),
            &endpoints(engine, base),
            engine,
            None,
            &query("fixture query"),
            &parse_query("fixture query"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            result.sources.first().map(|source| source.title.as_str()),
            Some(title)
        );
        if engine == "duckduckgo" {
            assert_eq!(result.sources.len(), 1);
            assert_eq!(result.sources[0].url, "https://docs.rs/tokio/latest/tokio/");
        }
        let requests = requests.lock().await;
        let sent = requests.first().unwrap();
        assert!(sent.headers.contains_key("user-agent"));
        match engine {
            "duckduckgo" => {
                assert_eq!(
                    sent.headers.get("content-type").unwrap(),
                    "application/x-www-form-urlencoded"
                );
                assert!(sent.body.contains("q=fixture+query"));
            }
            "google" => {
                assert!(sent.uri.contains("udm=14"));
                assert!(sent.uri.contains("tbs=qdr%3Aw"));
            }
            "mojeek" => {
                assert!(sent.uri.contains("since=week"));
                assert!(sent.uri.contains("t=5"));
            }
            _ => assert!(sent.uri.contains("q=fixture+query")),
        }
    }
    let (base, requests) = server_with_home(
        include_str!("../fixtures/startpage-search.html"),
        "text/html",
        Some(include_str!("../fixtures/startpage-home.html")),
    )
    .await;
    let result = search_engine(
        &reqwest::Client::new(),
        &endpoints("startpage", base),
        "startpage",
        None,
        &query("fixture query"),
        &parse_query("fixture query"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        result.sources.first().map(|source| source.title.as_str()),
        Some("Example Startpage")
    );
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body.contains("sc=fixture-session"));
    assert!(requests[1].body.contains("query=fixture+query"));
    assert!(requests[1].body.contains("with_date=w"));
}

#[tokio::test]
async fn startpage_live_block_page_is_not_reported_as_empty_results() {
    let blocked = include_str!("../fixtures/startpage-blocked-live.html");
    let (base, _) = server_with_home(blocked, "text/html", Some(blocked)).await;
    let result = search_engine(
        &reqwest::Client::new(),
        &endpoints("startpage", base),
        "startpage",
        None,
        &query("rust tokio documentation"),
        &parse_query("rust tokio documentation"),
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn public_search_merges_consensus_from_all_keyless_engines() {
    let fixture = include_str!("../fixtures/public-search.html");
    let mut overrides = BTreeMap::new();
    for engine in ["duckduckgo", "startpage", "ecosia", "google", "mojeek"] {
        let (base, _) = server(fixture, "text/html").await;
        overrides.insert(engine.to_owned(), base);
    }
    let endpoints = Endpoints::with_overrides(overrides);
    let result = search_engine(
        &reqwest::Client::new(),
        &endpoints,
        "public",
        None,
        &query("consensus"),
        &parse_query("consensus"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.provider, "public");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].url, "https://example.com/common");
}
