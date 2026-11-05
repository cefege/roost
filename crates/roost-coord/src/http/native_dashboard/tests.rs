//! Capability-route security tests, kept beside the handler for the file cap.

use super::*;

#[test]
fn the_dashboard_csp_is_opaque_origin_by_construction() {
    let csp = sandbox_csp("roost.example");
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"), "{csp}");
    assert!(csp.contains("connect-src 'self' http://roost.example"));
    assert!(csp.contains("frame-ancestors 'none'"));
}

#[test]
fn a_host_with_header_syntax_cannot_reach_the_csp() {
    for host in ["", "a b", "a;b", "a\"b", "a/b"] {
        let request = Request::builder()
            .uri("/omp-stats/x/")
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("a request");
        assert_eq!(public_origin(&request), None, "{host}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_stalled_upload_cannot_outlive_the_request_deadline() {
    let body = Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    let deadline = tokio::time::Instant::now() + REQUEST_LIFETIME;
    assert!(matches!(
        read_body(body, deadline).await,
        BodyRead::TimedOut
    ));
}
