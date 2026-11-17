//! Short-lived route proof for an operator-approved replacement origin.
//!
//! The host controller owns the challenge and its expiry. This endpoint proves
//! routing to this coordinator, not the trustworthiness of a relaying proxy.

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use roost_host::{ProcessEnv, supported_host_platform};

pub const ACCESS_CHALLENGE_PATH: &str = "/.well-known/roost-access-challenge";

pub fn is_challenge_request(method: &axum::http::Method, path: &str) -> bool {
    *method == axum::http::Method::GET && path == ACCESS_CHALLENGE_PATH
}

/// Only the approved host and requests without an Origin may read the proof.
pub fn approved_challenge(headers: &HeaderMap) -> Option<String> {
    let platform = supported_host_platform().ok()?;
    let challenge = roost_host::access::route_challenge::read_active_route_challenge(
        &ProcessEnv::new(),
        platform,
    )
    .ok()??;
    challenge_for_host(headers, challenge)
}

fn challenge_for_host(
    headers: &HeaderMap,
    challenge: roost_host::access::route_challenge::RouteChallenge,
) -> Option<String> {
    if headers.contains_key(header::ORIGIN) || headers.get_all(header::HOST).iter().count() != 1 {
        return None;
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let authority = challenge.origin.strip_prefix("https://")?;
    if host != authority {
        return None;
    }
    Some(challenge.token)
}

pub async fn access_challenge(headers: HeaderMap) -> Response {
    let mut response = match approved_challenge(&headers) {
        Some(token) => (StatusCode::OK, token).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_host::access::route_challenge::RouteChallenge;

    fn challenge() -> RouteChallenge {
        RouteChallenge {
            origin: "https://candidate.example".into(),
            token: "secret".into(),
            expires_at: 1,
        }
    }

    #[test]
    fn challenge_is_restricted_to_exact_host_without_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("candidate.example"));
        assert_eq!(
            challenge_for_host(&headers, challenge()).as_deref(),
            Some("secret")
        );
        headers.append(header::HOST, HeaderValue::from_static("candidate.example"));
        assert!(challenge_for_host(&headers, challenge()).is_none());
        headers.remove(header::HOST);
        headers.insert(header::HOST, HeaderValue::from_static("other.example"));
        assert!(challenge_for_host(&headers, challenge()).is_none());
        headers.insert(header::HOST, HeaderValue::from_static("candidate.example"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://candidate.example"),
        );
        assert!(challenge_for_host(&headers, challenge()).is_none());
    }

    #[test]
    fn admission_exception_is_only_for_the_get_challenge_path() {
        use axum::http::Method;
        assert!(is_challenge_request(&Method::GET, ACCESS_CHALLENGE_PATH));
        assert!(!is_challenge_request(&Method::POST, ACCESS_CHALLENGE_PATH));
        assert!(!is_challenge_request(&Method::GET, "/api/db-export"));
        assert!(!is_challenge_request(
            &Method::GET,
            "/roost.v1.CoordinatorService/AccessApply"
        ));
    }
}
