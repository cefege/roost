//! Regression coverage for external deployment evidence and secret-key refusal.
//! Exercises the deployment probe's pure validation boundaries.
//! Fixtures contain no credentials and make no Kubernetes calls.

use super::*;
use serde_json::json;

#[test]
fn requires_ready_flux_and_k3s() {
    assert!(verify_ready(&json!({"metadata":{"generation":2},"status":{"observedGeneration":1,"conditions":[{"type":"Ready","status":"True"}]}})).is_err());
    assert!(
        verify_k3s(&json!({"items":[{"status":{"nodeInfo":{"kubeletVersion":"v1.30.0"}}}]}))
            .is_err()
    );
    assert!(
        verify_k3s(&json!({"items":[{"status":{"nodeInfo":{"kubeletVersion":"v1.30.0+k3s1"}}}]}))
            .is_ok()
    );
}

#[test]
fn rejects_secret_access_keys() {
    assert!(validate_environment_keys("", "ROOST_COORDINATOR_DATABASE_URL\n").is_ok());
    assert!(validate_environment_keys("", "ROOST_COORDINATOR_PUBLIC_URL\n").is_err());
    assert!(validate_environment_keys("ROOST_", "WEB_PUBLIC_URL\n").is_err());
    assert!(validate_environment_keys("", "bad key\n").is_err());
}
