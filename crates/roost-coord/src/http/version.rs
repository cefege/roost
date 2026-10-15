//! `GET /version`: the release this coordinator is running, for the fleet.
//!
//! Workers follow the coordinator's release, so they poll this route rather than
//! GitHub's "latest". The answer is the build's own identity from
//! `roost_host::build_identity`. `version` is `ROOST_BUILD_VERSION` as the image
//! was built: the release tag for a tagged release (`v3.0.0-rc.37`), `dev` for a
//! source build, and the 40-hex commit SHA for an untagged container image. Only a
//! `v`-prefixed value is a release a worker can fetch; the worker refuses the rest.
//! Unauthenticated and never cached; it is mounted beside the probes, so the audit,
//! SPA and admission layers treat it the same way.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use roost_host::{BuildIdentity, ProcessEnv, build_identity};
use serde::Serialize;

use crate::http::listener::ListenerState;

/// The version route path.
pub const VERSION_PATH: &str = "/version";

/// The body of `GET /version`. Field order and names are the wire contract.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct VersionBody {
    version: String,
    git_sha: String,
}

/// The one route, for the listener to merge.
pub fn version_routes() -> Router<Arc<ListenerState>> {
    Router::new().route(VERSION_PATH, get(version))
}

async fn version() -> Response {
    let body = body_of(build_identity(&ProcessEnv::new()));
    ([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

/// The wire body for a build identity: the artifact version verbatim, and the
/// commit beside it.
fn body_of(identity: BuildIdentity) -> VersionBody {
    VersionBody {
        version: identity.artifact_version,
        git_sha: identity.build_sha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(artifact_version: &str, build_sha: &str) -> BuildIdentity {
        BuildIdentity {
            artifact_version: artifact_version.to_string(),
            build_sha: build_sha.to_string(),
            is_compiled: true,
        }
    }

    #[test]
    fn a_tagged_release_reports_its_tag_and_commit() {
        let body = body_of(identity(
            "v3.0.0-rc.37",
            "0123456789abcdef0123456789abcdef01234567",
        ));
        assert_eq!(body.version, "v3.0.0-rc.37");
        assert_eq!(body.git_sha, "0123456789abcdef0123456789abcdef01234567");
    }

    #[test]
    fn an_untagged_image_reports_its_commit_as_the_version() {
        let sha = "fedcba9876543210fedcba9876543210fedcba98";
        let body = body_of(identity(sha, sha));
        assert_eq!(body.version, sha);
        assert_eq!(
            serde_json::to_string(&body).ok().as_deref(),
            Some(format!(r#"{{"version":"{sha}","git_sha":"{sha}"}}"#).as_str())
        );
    }

    #[test]
    fn a_source_build_reports_dev() {
        let body = body_of(identity("dev", "abc123"));
        assert_eq!(body.version, "dev");
    }
}
