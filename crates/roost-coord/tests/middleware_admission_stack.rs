// The admission gate as the mounted stack applies it: what it refuses, what it
// admits, what it answers before the listener knows its own port, and what a
// refusal must NOT come to look like.
//
// The layer order is only observable across the whole stack, so every assertion
// here is made over a real socket against the real router. The fixture is
// `middleware_support`, the single owner of "a coordinator with a migrated
// database and a bound port".
mod middleware_support;

use middleware_support::{FOREIGN_HOST, FixtureConfig, ListenerFixture};

/// The browser's Sync socket.
const SYNC_WS: &str = "/ws/coord-sync";

/// The worker's raw link, for a fingerprint-shaped path segment.
const WORKER_WS: &str =
    "/ws/coord-worker/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The one request budget a session spends per minute, so a test can exhaust it.
const MISC_METRICS: &str = "/roost.v1.CoordinatorService/MiscMetrics";

/// A `Host` that is not this coordinator is refused on a WebSocket route, and
/// the coordinator's own authority is not.
///
/// The worker route answers `400 upgrade required` for a request that is not an
/// upgrade, which is what makes this a two-sided assertion: the same request
/// with a foreign `Host` never reaches the handler at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_host_is_refused_before_the_worker_upgrade_route_answers() {
    let fixture = ListenerFixture::start("host", FixtureConfig::default()).await;

    let refused = fixture.request("GET", WORKER_WS, &[("Host", FOREIGN_HOST)]);
    assert_eq!(refused.status, 403);
    assert_eq!(refused.body, "forbidden host");

    let own = fixture.get(WORKER_WS);
    assert_eq!(
        own.status, 400,
        "the coordinator's own authority must reach the route: {}",
        own.body
    );
    assert_eq!(own.body, "upgrade required");
}

/// A `Host` the operator declared as their front door is the coordinator's own,
/// because a request through that door arrives bearing its name. v2 admits it
/// for exactly this reason (`coordinator-request-admission.ts:25-26`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_front_door_is_an_admitted_host() {
    let fixture = ListenerFixture::start(
        "frontdoor",
        FixtureConfig {
            web_public_url: Some("https://desk.example.com".to_owned()),
            ..FixtureConfig::default()
        },
    )
    .await;

    let refused = fixture.request("GET", WORKER_WS, &[("Host", FOREIGN_HOST)]);
    assert_eq!(refused.status, 403);

    let through_the_door = fixture.request(
        "GET",
        WORKER_WS,
        &[
            ("Host", "desk.example.com"),
            ("Origin", "https://desk.example.com"),
        ],
    );
    assert_eq!(
        through_the_door.status, 400,
        "a declared front door is an admitted host: {}",
        through_the_door.body
    );
}

/// A listener that has not yet learned its port refuses EVERYTHING, on both
/// WebSocket routes and on Connect, with a body that says why.
///
/// Both sockets are asserted because the failure this guards is a mount that
/// covers one route and forgets the other, and a test that only checked the
/// worker route would pass with the Sync socket unprotected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpublished_listener_refuses_both_websocket_routes_and_connect() {
    let fixture = ListenerFixture::start(
        "prebind",
        FixtureConfig {
            publish_port: false,
            ..FixtureConfig::default()
        },
    )
    .await;

    for path in [SYNC_WS, WORKER_WS, MISC_METRICS] {
        let response = fixture.get(path);
        assert_eq!(response.status, 503, "{path} before the port is known");
        assert_eq!(response.body, "listener unavailable", "{path}");
    }
}

/// A refusal is not decorated as though it were an answer.
///
/// This is the layer ORDER, asserted across the stack: the security layer sits
/// below the gate, so a refused request carries no CSP, no `X-Frame-Options`
/// and no `Access-Control-Allow-Origin`. Move the gate below the security layer
/// and every one of those headers appears on a 403 -- and a browser then reads a
/// refused origin as one the coordinator will answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_request_carries_none_of_the_response_headers() {
    let fixture = ListenerFixture::start(
        "order",
        FixtureConfig {
            cors_allowed_origins: vec!["https://desk.example.com".to_owned()],
            ..FixtureConfig::default()
        },
    )
    .await;

    let refused = fixture.request(
        "GET",
        "/api/db-export",
        &[
            ("Host", FOREIGN_HOST),
            ("Origin", "https://desk.example.com"),
        ],
    );
    assert_eq!(refused.status, 403);
    for header in [
        "content-security-policy",
        "x-frame-options",
        "x-content-type-options",
        "access-control-allow-origin",
        "access-control-allow-methods",
    ] {
        assert_eq!(refused.header(header), None, "{header} on a refusal");
    }

    // The same request that IS admitted gets all of them, so the assertions
    // above are about the refusal and not about a layer that never runs.
    let admitted = fixture.request(
        "GET",
        "/api/db-export",
        &[("Origin", "https://desk.example.com")],
    );
    assert_ne!(admitted.status, 403);
    assert_eq!(admitted.header("x-frame-options"), Some("DENY"));
    assert_eq!(
        admitted.header("access-control-allow-origin"),
        Some("https://desk.example.com")
    );
}

/// The export route is the one route that answers a whole database, and it
/// answers a caller that arrived through the front door with a refusal.
///
/// The same listener answers the same request from the host itself, which is
/// what makes this about the trust profile rather than about the path: the
/// socket peer is loopback in both cases, and the only difference is the
/// forwarded header the front door adds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_route_refuses_a_caller_a_front_door_forwarded() {
    let fixture = ListenerFixture::start(
        "export",
        FixtureConfig {
            trust_proxy: true,
            ..FixtureConfig::default()
        },
    )
    .await;

    let forwarded = fixture.request("GET", "/api/db-export", &[("X-Forwarded-For", "203.0.113.9")]);
    assert_eq!(forwarded.status, 403);
    assert_eq!(forwarded.body, r#"{"error":"on-host only"}"#);

    let on_host = fixture.get("/api/db-export");
    assert_ne!(
        on_host.status, 403,
        "the same listener must still answer a caller on the host: {}",
        on_host.body
    );
}
