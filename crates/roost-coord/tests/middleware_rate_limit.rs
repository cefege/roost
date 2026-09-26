// The rate-limit mount as the stack applies it: which requests spend a budget,
// whose budget they spend, and that a CORS preflight is not one of them.
//
// Every request here is a real socket request against the real router, because
// the mount's two load-bearing facts are both about the stack: the limiter sits
// below the caller-origin layer whose address keys it, and below the preflight
// branch that answers before it.
mod middleware_support;

use middleware_support::{FixtureConfig, ListenerFixture};
use roost_coord::middleware::rate_limit::DEFAULT_TOKENS_PER_WINDOW;

/// A mutation surface with the default budget. It is refused by the auth gate,
/// which does not matter: the budget is spent before the gate, because the
/// budget is for the attempt.
const PAIR_CREATE: &str = "/roost.v1.CoordinatorService/PairCreate";

/// A read, which v2 does not limit: `MiscMetrics` is absent from
/// `RATE_LIMITED_METHODS`, and a test that re-listed the methods here would be
/// a second answer to which ones are limited.
const MISC_METRICS: &str = "/roost.v1.CoordinatorService/MiscMetrics";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_spends_one_budget_across_its_connections() {
    let fixture = ListenerFixture::start("limit", FixtureConfig::default()).await;

    // Each request is its own connection and therefore its own ephemeral port.
    // A key of the connection would hand every one of them a fresh budget and
    // the limit would never fire.
    for _ in 0..DEFAULT_TOKENS_PER_WINDOW {
        let response = fixture.request("POST", PAIR_CREATE, &[]);
        assert_ne!(response.status, 429, "the budget ran out early");
    }
    let refused = fixture.request("POST", PAIR_CREATE, &[]);
    assert_eq!(refused.status, 429);
    assert_eq!(refused.body, r#"{"error":"rate limit exceeded"}"#);
    assert!(
        refused.header("retry-after").is_some(),
        "a refusal has to say when to come back"
    );
}

/// A method nobody limited spends nothing, however many times it is called.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unlimited_method_never_opens_a_bucket() {
    let fixture = ListenerFixture::start("unlimited", FixtureConfig::default()).await;

    for _ in 0..(DEFAULT_TOKENS_PER_WINDOW * 2) {
        let response = fixture.request("POST", MISC_METRICS, &[]);
        assert_ne!(response.status, 429, "a read was charged a budget");
    }
}

/// A browser sends a preflight before every non-simple request. v2 answers
/// `OPTIONS` before it checks a budget, and charging the preflight would halve
/// every real caller's budget without limiting anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preflight_does_not_spend_the_budget_it_precedes() {
    let fixture = ListenerFixture::start("preflight", FixtureConfig::default()).await;

    for _ in 0..(DEFAULT_TOKENS_PER_WINDOW * 2) {
        let preflight = fixture.request(
            "OPTIONS",
            PAIR_CREATE,
            &[("Origin", middleware_support::WORKER_LOCAL_UI_ORIGIN)],
        );
        assert_eq!(preflight.status, 204);
    }
    let real = fixture.request("POST", PAIR_CREATE, &[]);
    assert_ne!(
        real.status, 429,
        "the preflights spent the budget the real requests need"
    );
}

/// The two WebSocket upgrades are long-lived sockets, not request budgets. A
/// 100/minute budget on one would break a terminal rather than protect
/// anything, so neither upgrade path spends a bucket however often it is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_websocket_upgrades_are_never_charged() {
    let fixture = ListenerFixture::start("sockets", FixtureConfig::default()).await;
    let sync = "/ws/coord-sync";
    let worker =
        "/ws/coord-worker/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    for _ in 0..(DEFAULT_TOKENS_PER_WINDOW * 2) {
        assert_ne!(fixture.get(sync).status, 429, "the Sync socket was charged");
        assert_ne!(
            fixture.get(worker).status,
            429,
            "the worker link was charged"
        );
    }
}
