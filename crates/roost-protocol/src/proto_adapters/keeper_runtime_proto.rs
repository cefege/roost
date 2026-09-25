//! The keeper runtime observation's protobuf form, which the coordinator
//! persists to decide admission. The contract itself lives in `keeper_update`:
//! this file only maps it, and re-checks the value in both directions so a wide
//! counter or an optional field cannot make the two sides disagree.
//!
//! The `bun_abi` field the message still carries is deliberately left unmapped:
//! a v3 keeper is not a Bun process, so there is nothing truthful to put in it.

use roost_proto::KeeperContractV1 as PbKeeperContractV1;
use roost_proto::KeeperRuntimeObservationV1 as PbKeeperRuntimeObservationV1;

use crate::keeper_update::{KeeperContractV1, KeeperRuntimeObservationV1};
use crate::{ProtocolError, ProtocolResult};

const OBSERVATION: &str = "keeper_observation";

fn to_wire(field: impl AsRef<str>, value: i64) -> ProtocolResult<u64> {
    u64::try_from(value).map_err(|_| {
        ProtocolError::new(field.as_ref(), format!("must not be negative, got {value}"))
    })
}

fn from_wire(field: impl AsRef<str>, value: u64) -> ProtocolResult<i64> {
    i64::try_from(value)
        .map_err(|_| ProtocolError::new(field.as_ref(), format!("must not exceed {value}")))
}

pub fn keeper_contract_to_proto(contract: &KeeperContractV1) -> ProtocolResult<PbKeeperContractV1> {
    contract.validate()?;
    Ok(PbKeeperContractV1 {
        protocol_version: contract.protocol_version,
        supported_features: contract.supported_features.clone(),
        required_features: contract.required_features.clone(),
        // A keeper that cannot prove which binary it is sends no digest, and
        // no restart may be admitted against it.
        implementation_digest: contract.implementation_digest.clone(),
        platform: contract.platform.clone(),
        arch: contract.arch.clone(),
        build_sha: contract.build_sha.clone(),
        ..Default::default()
    })
}

pub fn keeper_contract_from_proto(
    contract: &PbKeeperContractV1,
) -> ProtocolResult<KeeperContractV1> {
    let decoded = KeeperContractV1 {
        protocol_version: contract.protocol_version,
        supported_features: contract.supported_features.clone(),
        required_features: contract.required_features.clone(),
        implementation_digest: contract.implementation_digest.clone(),
        platform: contract.platform.clone(),
        arch: contract.arch.clone(),
        build_sha: contract.build_sha.clone(),
    };
    decoded.validate()?;
    Ok(decoded)
}

pub fn keeper_runtime_observation_to_proto(
    observation: &KeeperRuntimeObservationV1,
) -> ProtocolResult<PbKeeperRuntimeObservationV1> {
    let validated = KeeperRuntimeObservationV1::parse(
        &serde_json::to_value(observation)
            .map_err(|error| ProtocolError::new(OBSERVATION, error.to_string()))?,
    )?;
    Ok(PbKeeperRuntimeObservationV1 {
        schema_version: u32::from(validated.schema_version),
        running_contract: keeper_contract_to_proto(&validated.running_contract)?.into(),
        keeper_pid: to_wire(format!("{OBSERVATION}.keeper_pid"), validated.keeper_pid)?,
        keeper_epoch: validated.keeper_epoch.clone(),
        channel_count: validated.channel_count,
        binding_digest: validated.binding_digest.clone(),
        reconciled_at_ms: to_wire(
            format!("{OBSERVATION}.reconciled_at_ms"),
            validated.reconciled_at_ms,
        )?,
        ..Default::default()
    })
}

pub fn keeper_runtime_observation_from_proto(
    observation: &PbKeeperRuntimeObservationV1,
) -> ProtocolResult<KeeperRuntimeObservationV1> {
    // A proof without the contract it is proof of is not a proof, so this is an
    // error rather than a default contract nobody vouched for.
    // Two failures, two scopes, applied once each. The missing-contract error
    // carries its own path, so routing it through the same `map_err` as the
    // decode would print `keeper_observation.keeper_observation.…`.
    let running_contract = match observation.running_contract.as_option() {
        None => {
            return Err(ProtocolError::new(
                format!("{OBSERVATION}.running_contract"),
                "keeper runtime observation is missing its running contract",
            ));
        }
        Some(contract) => {
            keeper_contract_from_proto(contract).map_err(|error| error.within(OBSERVATION))?
        }
    };
    KeeperRuntimeObservationV1::parse(&serde_json::json!({
        "schema_version": observation.schema_version,
        "running_contract": running_contract,
        "keeper_pid": from_wire(format!("{OBSERVATION}.keeper_pid"), observation.keeper_pid)?,
        "keeper_epoch": observation.keeper_epoch,
        "channel_count": observation.channel_count,
        "binding_digest": observation.binding_digest,
        "reconciled_at_ms": from_wire(
            format!("{OBSERVATION}.reconciled_at_ms"),
            observation.reconciled_at_ms,
        )?,
    }))
}
