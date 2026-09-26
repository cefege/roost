//! What boot established that a handler needs later: the tenancy scope, the
//! resolved config, this process's identity and the instant it started.
//!
//! Owned by `CoordServices::boot`. `serve` fills it from the boot order
//! (`docs/phase3-coord-contract.md` §1.1); every handler reads it from
//! `core.services.boot` rather than taking it as a parameter, so adding a boot
//! fact widens this struct and not one handler signature per domain.
//!
//! A MISSING FACT IS A WIRING FAULT AND SAYS SO. `CoordServices::new` builds an
//! unbooted set, which a test may use, but a process that reached its listener
//! with a missing fact was constructed wrongly. Answering with a default would
//! read as "this deployment has no dashboard" or "this deployment runs on the
//! default config", both of which are beliefs a caller would then act on; the
//! `Internal` refusal names the fact and the boot step that fills it.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_host::CoordConfig;

use crate::auth::self_hosted_tenant::SelfHostedTenant;

/// The facts boot established, as a handler finds them.
#[derive(Debug, Clone, Default)]
pub struct BootFacts {
    /// The single account, organization and dashboard a self-hosted deployment
    /// is made of, or `None` before the tenancy invariant has run.
    pub tenant: Option<SelfHostedTenant>,
    /// The resolved, already-validated configuration, shared rather than
    /// cloned per handler.
    pub config: Option<Arc<CoordConfig>>,
    /// A fresh identity for this process, so a log line tells a restart from a
    /// reconnect.
    pub process_epoch: String,
    /// When this process started, in epoch milliseconds.
    pub boot_ms: i64,
}

impl BootFacts {
    /// The facts a coordinator that has not booted has: nothing, empty, zero.
    ///
    /// The same value `Default` derives, named for the one caller that means
    /// it — `CoordServices::new`, which a test builds and `serve` does not.
    #[must_use]
    pub fn unbooted() -> Self {
        Self::default()
    }

    /// The tenancy scope, or the boot step that fills it.
    pub fn require_tenant(&self) -> Result<&SelfHostedTenant, ConnectError> {
        self.tenant.as_ref().ok_or_else(|| missing("tenant"))
    }

    /// The resolved config, or the boot step that fills it.
    pub fn require_config(&self) -> Result<&CoordConfig, ConnectError> {
        self.config.as_deref().ok_or_else(|| missing("config"))
    }

    /// This process's identity, empty only on an unbooted set.
    #[must_use]
    pub fn process_epoch(&self) -> &str {
        &self.process_epoch
    }

    /// When this process started, zero only on an unbooted set.
    #[must_use]
    pub fn boot_ms(&self) -> i64 {
        self.boot_ms
    }
}

/// The one wording every missing boot fact is refused with.
///
/// `Internal`, and not a value the caller could mistake for an absent feature:
/// the request is well-formed and the coordinator is misassembled.
fn missing(fact: &str) -> ConnectError {
    ConnectError::new(
        ErrorCode::Internal,
        format!("coordinator booted without {fact}"),
    )
}
