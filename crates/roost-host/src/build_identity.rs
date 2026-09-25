//! Compile-time build identity for a self-contained Roost binary.
//!
//! A source checkout falls back to the stamp its service provides; a compiled
//! process always reports the artifact's own immutable identity, so a binary
//! replaced in place cannot describe itself as whatever it used to be. The
//! compile-time values arrive as environment variables set when the crate is
//! compiled — a build script that emits `cargo::rustc-env`, or an exporting
//! release pipeline — and the runtime fallback is what a source run uses.

use crate::env::EnvSource;

/// The version stamped into the binary when it was compiled.
pub const COMPILED_ROOST_ARTIFACT_VERSION: Option<&str> = option_env!("ROOST_BUILD_VERSION");

/// The commit stamped into the binary when it was compiled. Its presence is
/// also what marks the binary as a compiled build.
pub const COMPILED_ROOST_BUILD_SHA: Option<&str> = option_env!("ROOST_BUILD_SHA");

/// The stamp a build with no version reports.
pub const DEV_BUILD_STAMP: &str = "dev";

/// The commit a service-provided stamp is read from, in preference order.
pub const GIT_SHA_ENV: &str = "GIT_SHA";

/// The Roost-namespaced spelling of [`GIT_SHA_ENV`].
pub const ROOST_GIT_SHA_ENV: &str = "ROOST_GIT_SHA";

/// What a process reports about the artifact it is running from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    /// The release version, or `dev` for a source checkout.
    pub artifact_version: String,
    /// The commit, or the service-provided stamp for a source checkout.
    pub build_sha: String,
    /// Whether the binary carried its own commit stamp.
    pub is_compiled: bool,
}

/// The identity of this process, with a source checkout's stamp read from the
/// environment its service provided.
pub fn build_identity(env: &dyn EnvSource) -> BuildIdentity {
    BuildIdentity {
        artifact_version: COMPILED_ROOST_ARTIFACT_VERSION
            .unwrap_or(DEV_BUILD_STAMP)
            .to_string(),
        build_sha: COMPILED_ROOST_BUILD_SHA
            .map(str::to_string)
            .or_else(|| env.get(GIT_SHA_ENV))
            .or_else(|| env.get(ROOST_GIT_SHA_ENV))
            .unwrap_or_else(|| DEV_BUILD_STAMP.to_string()),
        is_compiled: COMPILED_ROOST_BUILD_SHA.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COMPILED_ROOST_BUILD_SHA, DEV_BUILD_STAMP, GIT_SHA_ENV, ROOST_GIT_SHA_ENV, build_identity,
    };
    use crate::env::MapEnv;

    #[test]
    fn a_compiled_stamp_always_wins_over_the_environment() {
        // A binary replaced in place must describe itself as what it is, so a
        // stale `GIT_SHA` in the launching service cannot rename the release.
        let env = MapEnv::new()
            .with(GIT_SHA_ENV, "0123456789ab")
            .with(ROOST_GIT_SHA_ENV, "ba9876543210");
        let identity = build_identity(&env);
        let Some(stamped) = COMPILED_ROOST_BUILD_SHA else {
            // Not a compiled build; the fallback chain below is what applies.
            return;
        };
        assert_eq!(identity.build_sha, stamped);
        assert!(identity.is_compiled);
    }

    #[test]
    fn a_source_checkout_takes_its_stamp_from_the_service_in_order() {
        if COMPILED_ROOST_BUILD_SHA.is_some() {
            return;
        }
        assert_eq!(
            build_identity(&MapEnv::new()).build_sha,
            DEV_BUILD_STAMP,
            "an unstamped source run reports dev"
        );
        assert_eq!(
            build_identity(&MapEnv::new().with(ROOST_GIT_SHA_ENV, "ba9876543210")).build_sha,
            "ba9876543210"
        );
        assert_eq!(
            build_identity(
                &MapEnv::new()
                    .with(ROOST_GIT_SHA_ENV, "ba9876543210")
                    .with(GIT_SHA_ENV, "0123456789ab")
            )
            .build_sha,
            "0123456789ab",
            "GIT_SHA is the service's own stamp and outranks the Roost spelling"
        );
    }

    #[test]
    fn a_source_checkout_reports_the_development_version() {
        assert_eq!(
            build_identity(&MapEnv::new().with(GIT_SHA_ENV, "0123456789ab")).artifact_version,
            DEV_BUILD_STAMP
        );
    }
}
