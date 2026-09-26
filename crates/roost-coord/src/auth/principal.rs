//! The principal a verified key resolves to, and the rules that refuse one.
//!
//! Owned by the coordinator's auth layer. `jwt_verify` answers "which key";
//! this file answers "what may that key do", and the distinction is load-bearing:
//! a valid signature is proof of a key, never proof of a role.
//!
//! THREE KINDS, NOT TWO. `AccountDevice` is a browser. `Worker` is a machine.
//! `LegacySelfHosted` is a third, browser-capable kind that exists only for a
//! key registered before accounts existed
//! (`apps/coord/src/auth/auth-principal.ts:21-23`). It is accepted by
//! `require_account_device` and **refused at the Sync WebSocket with 404**
//! (`apps/coord/src/sync/sync-ws-upgrade.ts:162-170`) because it carries no
//! runtime identity to scope a feed to. A port that folds it into
//! `AccountDevice` gives an unscoped key a scoped feed.
//!
//! WHY A DUAL-AUTHORITY ROW IS REFUSED RATHER THAN PICKED. If a fingerprint is
//! both a `workers.fp` and an `account_devices.fingerprint`,
//! `resolveCallerPrincipal` returns `None` (`auth-principal.ts:66`): "A key must
//! never acquire two kinds of authority." Picking one silently would make the
//! answer depend on row order, and the answer decides whether a socket may
//! write. The same self-hosted-tenant boot invariant refuses the same condition
//! before any RPC runs (§4.8 of `docs/phase3-coord-contract.md`), so reaching
//! this at runtime means the database changed under a running process.

use roost_protocol::{ProtocolError, ProtocolResult};

/// What a verified key is authorized as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// A browser: a key associated with an account device.
    AccountDevice {
        /// The authorized key's fingerprint.
        fingerprint: String,
        /// The operator-facing label on the key row.
        label: String,
        /// The account this key belongs to.
        account_id: String,
    },
    /// A machine: a key that is a registered worker's identity.
    Worker {
        /// The worker fingerprint, which is also the key's fingerprint.
        fingerprint: String,
        /// The operator-facing label on the key row.
        label: String,
    },
    /// A pre-account browser key. Carries no account and no scope.
    LegacySelfHosted {
        /// The authorized key's fingerprint.
        fingerprint: String,
        /// The operator-facing label on the key row.
        label: String,
    },
}

impl Principal {
    /// The key fingerprint, whichever kind this is.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        match self {
            Principal::AccountDevice { fingerprint, .. }
            | Principal::Worker { fingerprint, .. }
            | Principal::LegacySelfHosted { fingerprint, .. } => fingerprint,
        }
    }

    /// The key label, whichever kind this is.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Principal::AccountDevice { label, .. }
            | Principal::Worker { label, .. }
            | Principal::LegacySelfHosted { label, .. } => label,
        }
    }

    /// Whether this principal may act as a browser.
    ///
    /// Includes [`Principal::LegacySelfHosted`], which is why that kind is not
    /// merely an `AccountDevice` with an empty account: folding them together
    /// would lose the ability to refuse a legacy key at the Sync upgrade.
    #[must_use]
    pub fn is_browser(&self) -> bool {
        matches!(
            self,
            Principal::AccountDevice { .. } | Principal::LegacySelfHosted { .. }
        )
    }

    /// Whether this principal may act as a machine.
    #[must_use]
    pub fn is_worker(&self) -> bool {
        matches!(self, Principal::Worker { .. })
    }

    /// Refuse anything that is not a browser.
    ///
    /// The message and the marker header are the contract a browser reads:
    /// `Unauthenticated: authentication required` with
    /// `x-roost-auth-layer: device`
    /// (`apps/coord/src/auth/auth-interceptor.ts:256-262`). A worker principal
    /// hitting a device-only path gets the same answer, because telling it "you
    /// are a worker" would confirm the key is live to a peer that only guessed
    /// the fingerprint.
    pub fn require_account_device(&self) -> ProtocolResult<&str> {
        if self.is_browser() {
            Ok(self.fingerprint())
        } else {
            Err(ProtocolError::new(
                "auth.principal",
                "authentication required",
            ))
        }
    }

    /// Refuse anything that is not a machine.
    pub fn require_worker(&self) -> ProtocolResult<&str> {
        if self.is_worker() {
            Ok(self.fingerprint())
        } else {
            Err(ProtocolError::new(
                "auth.principal",
                "authentication required",
            ))
        }
    }
}

/// The header that names which auth layer refused a request.
///
/// Without it a browser sees an opaque 401 and cannot tell "log in again" from
/// "this method needs a device credential". It is `access-control-expose-headers`
/// -listed for exactly that reason
/// (`apps/coord/src/middleware/security.ts:63`).
pub const AUTH_LAYER_HEADER: &str = "x-roost-auth-layer";

/// The value of [`AUTH_LAYER_HEADER`] on a device-layer refusal.
pub const AUTH_LAYER_DEVICE: &str = "device";

/// The rows a `resolveCallerPrincipal` join found, before the rules apply.
///
/// Taken as data so the refusals are testable with no database, which is the
/// whole point: these are five boolean decisions and each one has a reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalFacts {
    /// A row exists in `authorized_keys`.
    pub authorized_key: bool,
    /// A row exists in `account_devices` for this key.
    pub account_device: bool,
    /// The account's status string, when a device row exists.
    pub account_status: Option<String>,
    /// The account id, when a device row exists.
    pub account_id: Option<String>,
    /// A row exists in `workers` for this key.
    pub worker: bool,
    /// `workers.deleted_at_ms` is not null: a tombstoned worker.
    pub worker_tombstoned: bool,
    /// The label on the `authorized_keys` row.
    pub label: String,
}

/// The account status that may authenticate. Anything else is not an account a
/// credential can act for.
const ACCOUNT_STATUS_ACTIVE: &str = "active";

/// Resolve verified key facts into exactly one principal, or refuse.
///
/// Refusal order is the order the questions are asked, and each refusal is a
/// distinct reason so an operator can tell a tombstoned worker from a
/// disabled account.
pub fn resolve_principal(fingerprint: &str, facts: &PrincipalFacts) -> ProtocolResult<Principal> {
    if !facts.authorized_key {
        return Err(ProtocolError::new(
            "auth.principal",
            format!("no authorized key for {fingerprint}"),
        ));
    }
    if facts.account_device && facts.worker {
        return Err(ProtocolError::new(
            "auth.principal",
            "authorized key is both a worker and an account device",
        ));
    }
    if facts.worker {
        if facts.worker_tombstoned {
            return Err(ProtocolError::new("auth.principal", "worker is deleted"));
        }
        return Ok(Principal::Worker {
            fingerprint: fingerprint.to_string(),
            label: facts.label.clone(),
        });
    }
    if facts.account_device {
        let Some(account_id) = facts.account_id.as_deref() else {
            return Err(ProtocolError::new(
                "auth.principal",
                "account device has no account",
            ));
        };
        if facts.account_status.as_deref() != Some(ACCOUNT_STATUS_ACTIVE) {
            return Err(ProtocolError::new(
                "auth.principal",
                "account is not active",
            ));
        }
        return Ok(Principal::AccountDevice {
            fingerprint: fingerprint.to_string(),
            label: facts.label.clone(),
            account_id: account_id.to_string(),
        });
    }
    Ok(Principal::LegacySelfHosted {
        fingerprint: fingerprint.to_string(),
        label: facts.label.clone(),
    })
}
