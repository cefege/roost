//! What a rotation asks the coordinator, and how a leftover rotation is read.
//!
//! Split out of the lifecycle so the decision table can be read — and tested —
//! without a key store, a clock or a probe behind it. The one function that
//! matters here is [`recover_rotation`], because it is the whole of "a rotated
//! key has a window in which both are valid and one in which only the new one
//! is", and getting its order wrong locks a device out of its own account.
//!
//! Ported from `apps/web/src/client/auth/web-key.ts:128-158`.

use std::fmt;

use crate::client::auth::keystore::{KeyAdmission, KeyStoreError};

/// Asks the coordinator whether a credential's key is an authorized device.
///
/// The one question the rotation recovery and the reset both turn on, and it is
/// a trait rather than a Connect call because the answer needs a round trip this
/// crate must not make. A host implements it with the SAME client it uses for
/// everything else; there is no second transport here.
pub trait DeviceKeyProbe {
    /// Present `bearer` to the coordinator and classify the answer.
    ///
    /// An unreachable coordinator is [`KeyAdmission::Ambiguous`], never
    /// [`KeyAdmission::DeviceRejected`]: the two decisions that read this
    /// promote and delete, and only the second is recoverable.
    fn probe_bearer(&self, bearer: &str) -> KeyAdmission;
}

/// What a rotation asks the coordinator to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationRequest {
    /// The replacement key's public half, standard base64, as `ssh_pubkey_b64`.
    pub public_key_b64: String,
    /// The human label the device list will carry.
    pub label: String,
    /// The current key's credential, or `None` when it could not be signed.
    ///
    /// `None` is a real state and the request still goes out: a device that
    /// cannot sign is a device whose call still has to reach the coordinator to
    /// be told why. That rule is `client::auth::bearer_for_signing`'s.
    pub bearer: Option<String>,
}

/// A rotation the coordinator considered and refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationRefusal {
    /// The coordinator's status text.
    pub message: String,
    /// Whether this is a considered answer rather than a transport failure.
    ///
    /// The same distinction `redeem` draws, and for the same reason: a
    /// `PermissionDenied` is final and a timeout is worth another attempt.
    pub authoritative: bool,
}

/// Asks the coordinator to make a new public key this device's current key.
pub trait DeviceKeyRotator {
    /// Perform `DevicesRotateCurrent` and return the new device's fingerprint.
    fn rotate_current(&mut self, request: &RotationRequest) -> Result<String, RotationRefusal>;
}

/// Why a rotation did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationError {
    /// The local key store could not carry out its half.
    Key(KeyStoreError),
    /// The coordinator refused.
    Refused(RotationRefusal),
}

impl fmt::Display for RotationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(error) => write!(formatter, "{error}"),
            Self::Refused(refusal) => write!(
                formatter,
                "the coordinator refused the rotation: {}",
                refusal.message
            ),
        }
    }
}

impl std::error::Error for RotationError {}

impl From<KeyStoreError> for RotationError {
    fn from(error: KeyStoreError) -> Self {
        Self::Key(error)
    }
}

/// What a rotation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationOutcome {
    /// A rotation this browser had started before was finished and promoted.
    Recovered,
    /// A new key was generated, enrolled, and promoted.
    Rotated,
}

/// What a reset did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// An interrupted rotation was resolved; the device key is unchanged.
    Recovered,
    /// There was no device key to remove.
    NotPaired,
    /// The revoked key was removed, and this browser is unpaired.
    Unpaired,
}

/// What a leftover rotation stage means, given what the coordinator said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationRecovery {
    /// No rotation was in progress.
    None,
    /// The staged key is already the device key: promote it.
    Promoted,
    /// The staged key is rejected and the current one still works: drop the
    /// stage, keep the device.
    Discarded,
    /// The coordinator could not be asked, or the two answers disagree.
    Ambiguous,
}

/// Decide what a staged rotation means.
///
/// A pure function over the two probe answers, and the only place that mapping
/// exists — the lifecycle performs the probes and applies the result, so the
/// rule cannot be written twice with two different orderings.
///
/// `current` is `None` when it was not probed, which is a decision in itself:
/// once the staged key is authorized the old key's state cannot change the
/// answer, so it is not asked. Every other branch needs BOTH answers, and both
/// bounds are checked here rather than one. A rejected stage with an
/// unreachable current key is `Ambiguous`, not `Discarded`, because discarding on
/// an unanswered question is how a working device key gets deleted; and an
/// ambiguous stage is never `Promoted`, whatever the old key says, because
/// promoting on an unanswered question is how an unauthorized key gets installed.
pub const fn recover_rotation(
    staged: KeyAdmission,
    current: Option<KeyAdmission>,
) -> RotationRecovery {
    match staged {
        KeyAdmission::Authorized => RotationRecovery::Promoted,
        KeyAdmission::Ambiguous => RotationRecovery::Ambiguous,
        KeyAdmission::DeviceRejected => match current {
            Some(KeyAdmission::Authorized) => RotationRecovery::Discarded,
            Some(KeyAdmission::Ambiguous) | Some(KeyAdmission::DeviceRejected) | None => {
                RotationRecovery::Ambiguous
            }
        },
    }
}
