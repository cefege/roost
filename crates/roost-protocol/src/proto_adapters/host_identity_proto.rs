//! The host identity triple's protobuf form. Worker registration and heartbeat
//! use it, and a persisted worker record omits a null identity entirely.
//!
//! Both directions run the value through `normalize_host_identity`, because
//! that is the security boundary: a host field reaches the database and the
//! browser from whatever the operating system reported, so it is reduced to a
//! bounded, single-line display value on the way in and again on the way out.
//! An identity of three nulls describes no machine, so it travels as an empty
//! message rather than as a row that says nothing.

use roost_proto::HostIdentity as PbHostIdentity;

use crate::wire::worker::{HostIdentity, normalize_host_identity};

/// The one normalization both directions share. The normalizer reads the
/// snake_case keys the JSON contract uses, so the value is spelled here once
/// instead of at each call site.
fn normalized_identity(
    hardware_model: Option<&str>,
    chip: Option<&str>,
    linux_distribution: Option<&str>,
) -> Option<HostIdentity> {
    normalize_host_identity(&serde_json::json!({
        "hardware_model": hardware_model,
        "chip": chip,
        "linux_distribution": linux_distribution,
    }))
}

/// A missing identity and an identity that normalizes to nothing are the same
/// message, which is how a worker attests that it collects no host fields.
pub fn host_identity_to_proto(identity: Option<&HostIdentity>) -> PbHostIdentity {
    let normalized = identity.and_then(|identity| {
        normalized_identity(
            identity.hardware_model.as_deref(),
            identity.chip.as_deref(),
            identity.linux_distribution.as_deref(),
        )
    });
    PbHostIdentity {
        hardware_model: normalized.as_ref().and_then(|id| id.hardware_model.clone()),
        chip: normalized.as_ref().and_then(|id| id.chip.clone()),
        linux_distribution: normalized
            .as_ref()
            .and_then(|id| id.linux_distribution.clone()),
        ..Default::default()
    }
}

pub fn host_identity_from_proto(identity: Option<&PbHostIdentity>) -> Option<HostIdentity> {
    let identity = identity?;
    normalized_identity(
        identity.hardware_model.as_deref(),
        identity.chip.as_deref(),
        identity.linux_distribution.as_deref(),
    )
}
