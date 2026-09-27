//! The front door as a browser meets it: a deep link is a page, a bundle is a
//! bundle, and neither of them is allowed to swallow the surfaces a coordinator
//! answers underneath.
//!
//! Written against the REAL router on a real port rather than against the
//! resolver, because every rule here is about the order the surfaces are tried
//! in — and a resolver test cannot see a Connect POST that no longer arrives.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "middleware_support/mod.rs"]
mod middleware_support;

use middleware_support::{FixtureConfig, ListenerFixture};

async fn serving_dist() -> ListenerFixture {
    ListenerFixture::start(
        "spa",
        FixtureConfig {
            serve_dist: true,
            ..FixtureConfig::default()
        },
    )
    .await
}

#[tokio::test]
async fn a_deep_link_is_the_shell_and_a_bundle_is_the_bundle() {
    let fixture = serving_dist().await;

    for path in ["/", "/s/01HQ8Z", "/t/ab12cd34/", "/browse/ab12cd34"] {
        let response = fixture.get(path);
        assert_eq!(response.status, 200, "{path} is a page, not a miss");
        assert_eq!(
            response.header("content-type"),
            Some("text/html; charset=utf-8"),
            "{path} must reach the browser as the shell"
        );
        assert!(response.body.contains("<title>roost</title>"));
    }

    let bundle = fixture.get("/assets/app.a1b2c3.js");
    assert_eq!(bundle.status, 200);
    assert_eq!(
        bundle.header("content-type"),
        Some("application/javascript; charset=utf-8"),
        "a bundle served as HTML is a module the browser cannot parse"
    );
    assert!(bundle.body.contains("export const shell"));
    // The three cache rules are three different deployments, and a client that
    // cannot tell them apart is how yesterday's shell survives a deploy.
    assert_eq!(
        fixture.get("/").header("cache-control"),
        Some("no-cache, no-store, must-revalidate")
    );
    assert_eq!(
        bundle.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(fixture.get("/favicon.ico").header("cache-control"), Some("no-cache"));
}

#[tokio::test]
async fn a_missing_bundle_is_a_404_and_never_the_shell() {
    let fixture = serving_dist().await;
    let response = fixture.get("/assets/gone.deadbe.js");
    assert_eq!(response.status, 404);
    assert!(
        !response.body.contains("<title>roost</title>"),
        "a content-hashed bundle that is gone must not download as a page"
    );
    assert_eq!(fixture.get("/").status, 200, "the shell itself still serves");
}

#[tokio::test]
async fn a_client_that_admits_gzip_gets_one_and_one_that_does_not_does_not() {
    let fixture = serving_dist().await;
    let compressed = fixture.request(
        "GET",
        "/assets/app.a1b2c3.js",
        &[("accept-encoding", "gzip, deflate, br")],
    );
    assert_eq!(compressed.status, 200);
    assert_eq!(compressed.header("content-encoding"), Some("gzip"));
    assert_eq!(compressed.header("vary"), Some("accept-encoding"));
    // The body really is gzip: a decompressor that reads the magic bytes is the
    // only way to prove the header and the bytes agree.
    assert_eq!(&compressed.raw_body[..2], &[0x1f, 0x8b]);
    assert!(
        !compressed.body.contains("export const shell"),
        "a raw body under a gzip header is a bundle the browser cannot run"
    );

    let identity = fixture.request(
        "GET",
        "/assets/app.a1b2c3.js",
        &[("accept-encoding", "identity")],
    );
    assert_eq!(identity.header("content-encoding"), None);
    assert!(identity.body.contains("export const shell"));
    // `vary` rides with the identity answer too: a shared cache that kept it
    // would hand it to the client above.
    assert_eq!(identity.header("vary"), Some("accept-encoding"));
}

#[tokio::test]
async fn an_rpc_path_still_reaches_connect() {
    let fixture = serving_dist().await;
    // A Connect method the coordinator answers, and one it does not: both must
    // come back as RPC answers. A `200 text/html` here would mean the front
    // door had swallowed the entire API behind a page.
    let answered = fixture.request(
        "POST",
        "/roost.v1.CoordinatorService/MiscHealth",
        &[
            ("content-type", "application/json"),
            ("connect-protocol-version", "1"),
        ],
    );
    assert_eq!(
        answered.header("content-type").map(str::to_owned),
        Some("application/json".to_owned()),
        "Connect answered something other than a Connect body: {}",
        answered.body
    );
    assert!(
        !answered.body.contains("<title>roost</title>"),
        "an RPC path was resolved as a static page"
    );

    // The retired Sync is a mounted route in front of Connect, and a page path
    // must not be able to reach it either.
    let retired = fixture.request("POST", "/roost.v1.CoordinatorService/Sync", &[]);
    assert_eq!(retired.status, 410);
}

#[tokio::test]
async fn the_api_namespace_and_the_sockets_are_not_pages() {
    let fixture = serving_dist().await;
    // `/api/db-export` is a route that refuses anything not on this host; what
    // matters here is that the front door did not answer it with a page, which
    // is what a build present and no route would produce.
    let export = fixture.get("/api/db-export");
    assert_ne!(
        export.header("content-type"),
        Some("text/html; charset=utf-8"),
        "the export route must not be answered as a static page"
    );
    let unknown_api = fixture.get("/api/nope");
    assert_eq!(unknown_api.status, 404);

    // A socket handshake is not a page request: the upgrade either happens or
    // it is refused, and neither answer is a 200 with the shell in it.
    let socket = fixture.request("GET", "/ws/coord-sync", &[]);
    assert!(
        !socket.body.contains("<title>roost</title>"),
        "a WebSocket upgrade must not be resolved against a build"
    );
}

#[tokio::test]
async fn a_page_path_that_is_not_a_get_or_a_head_is_refused() {
    let fixture = serving_dist().await;
    for method in ["POST", "PUT", "DELETE"] {
        let response = fixture.request(method, "/s/01HQ8Z", &[]);
        assert_eq!(response.status, 405, "{method} on a page path");
        assert!(
            !response.body.contains("<title>roost</title>"),
            "{method} on a page path must not quietly load the app"
        );
    }
    // HEAD is the other half of the rule: the headers a GET would send, and no
    // body to send them with.
    let head = fixture.request("HEAD", "/assets/app.a1b2c3.js", &[]);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty(), "HEAD answered with a body");
    assert_eq!(
        head.header("content-type"),
        Some("application/javascript; charset=utf-8")
    );
}

#[tokio::test]
async fn a_coordinator_with_no_build_404s_every_page_and_says_so() {
    let fixture = ListenerFixture::start("spa-absent", FixtureConfig::default()).await;
    for path in ["/", "/s/01HQ8Z", "/assets/app.a1b2c3.js"] {
        assert_eq!(
            fixture.get(path).status,
            404,
            "{path} must be a miss when there is no build, and the boot log has \
             already said why"
        );
    }
    // A missing build is not a broken API.
    let rpc = fixture.request(
        "POST",
        "/roost.v1.CoordinatorService/MiscHealth",
        &[("content-type", "application/json")],
    );
    assert_eq!(rpc.status, 200);
}
