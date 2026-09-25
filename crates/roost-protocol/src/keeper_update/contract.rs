//! The keeper contract shapes and the validators that admit them.
//!
//! Split from the admission logic because the two answer different questions:
//! this file decides whether a value is a well-formed contract, observation or
//! journal entry, and the parent module decides what those values permit. Each
//! `parse` is the `…Schema.parse` it replaces: the derived `Deserialize` with
//! `deny_unknown_fields` is this contract's `.strict()`, and the checks after
//! it are the refinements Zod ran as `.refine` and `.superRefine`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    KEEPER_EMPTY_BINDING_DIGEST, KEEPER_UPDATE_CLASSIFICATIONS, KEEPER_UPDATE_REQUIRED_ACTIONS,
    PRESERVE, REPLACE_EMPTY, SHA256_DIGEST_LENGTH, WORKER_ONLY_SAFE,
    keeper_contracts_same_implementation,
};
use crate::ProtocolResult;
use crate::error::ProtocolError;
use crate::validate::{hex_of_len, integer_in_range, max_utf8_bytes, non_empty, one_of, uuid};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const CONTRACT_PROTOCOL_VERSION_MAX: i64 = 0xffff_ffff;
const CONTRACT_MAX_FEATURES: usize = 32;
const CONTRACT_MAX_FEATURE_LENGTH: usize = 64;
const CONTRACT_MAX_ARCH_LENGTH: usize = 64;
const CONTRACT_MAX_BUILD_SHA_LENGTH: usize = 128;
const CONTRACT_PLATFORMS: [&str; 3] = ["darwin", "linux", "win32"];
const KEEPER_CHANNEL_COUNT_MAX: i64 = 0xffff;

/// What a keeper binary reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct KeeperContractV1 {
    pub protocol_version: u32,
    pub supported_features: Vec<String>,
    pub required_features: Vec<String>,
    /// SHA-256 of the keeper binary. Null means the binary cannot prove what it
    /// is, and no restart may be admitted against it.
    pub implementation_digest: Option<String>,
    pub platform: String,
    pub arch: String,
    /// Release provenance, deliberately excluded from restart admission: a
    /// rebuild of identical bytes may carry a different sha and still be the
    /// keeper a live PTY belongs to.
    pub build_sha: String,
}

impl KeeperContractV1 {
    pub fn parse(value: &Value) -> ProtocolResult<Self> {
        let contract: Self = decode(value, "keeper_contract")?;
        contract.validate()?;
        Ok(contract)
    }

    /// The refinements, run on every contract this crate is handed — including
    /// the one nested inside a runtime observation.
    pub fn validate(&self) -> ProtocolResult<()> {
        let path = "keeper_contract";
        integer_in_range(
            &field(path, "protocol_version"),
            i64::from(self.protocol_version),
            1,
            CONTRACT_PROTOCOL_VERSION_MAX,
        )?;
        validate_sorted_features(&field(path, "supported_features"), &self.supported_features)?;
        validate_sorted_features(&field(path, "required_features"), &self.required_features)?;
        if let Some(digest) = &self.implementation_digest {
            hex_of_len(
                &field(path, "implementation_digest"),
                digest,
                SHA256_DIGEST_LENGTH,
            )?;
        }
        one_of(
            &field(path, "platform"),
            &self.platform,
            &CONTRACT_PLATFORMS,
        )?;
        bounded(&field(path, "arch"), &self.arch, CONTRACT_MAX_ARCH_LENGTH)?;
        bounded(
            &field(path, "build_sha"),
            &self.build_sha,
            CONTRACT_MAX_BUILD_SHA_LENGTH,
        )?;
        Ok(())
    }
}

/// A running keeper's own account of itself, reconciled by the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct KeeperRuntimeObservationV1 {
    pub schema_version: u8,
    pub running_contract: KeeperContractV1,
    pub keeper_pid: i64,
    pub keeper_epoch: String,
    pub channel_count: u32,
    pub binding_digest: String,
    pub reconciled_at_ms: i64,
}

impl KeeperRuntimeObservationV1 {
    pub fn parse(value: &Value) -> ProtocolResult<Self> {
        let observation: Self = decode(value, "keeper_observation")?;
        if observation.schema_version != 1 {
            return Err(ProtocolError::new(
                "keeper_observation.schema_version",
                "must be 1",
            ));
        }
        integer_in_range(
            "keeper_observation.keeper_pid",
            observation.keeper_pid,
            1,
            MAX_SAFE_INTEGER,
        )?;
        uuid("keeper_observation.keeper_epoch", &observation.keeper_epoch)?;
        integer_in_range(
            "keeper_observation.channel_count",
            i64::from(observation.channel_count),
            0,
            KEEPER_CHANNEL_COUNT_MAX,
        )?;
        hex_of_len(
            "keeper_observation.binding_digest",
            &observation.binding_digest,
            SHA256_DIGEST_LENGTH,
        )?;
        integer_in_range(
            "keeper_observation.reconciled_at_ms",
            observation.reconciled_at_ms,
            1,
            MAX_SAFE_INTEGER,
        )?;
        observation
            .running_contract
            .validate()
            .map_err(|error| error.within("keeper_observation.running_contract"))?;
        observation.validate()?;
        Ok(observation)
    }

    /// The cross-field rule a channel count and a binding digest share.
    ///
    /// A proof whose own two halves disagree is evidence of nothing, so it
    /// classifies as unproven rather than as an empty keeper.
    pub fn validate(&self) -> ProtocolResult<()> {
        let empty = self.channel_count == 0;
        if empty != (self.binding_digest == KEEPER_EMPTY_BINDING_DIGEST) {
            return Err(ProtocolError::new(
                "keeper_observation.binding_digest",
                "keeper channel count and binding digest disagree",
            ));
        }
        Ok(())
    }
}

/// The decision a deploy records before it touches a running keeper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct KeeperUpdateAdmissionV1 {
    pub classification: String,
    pub source_contract_digest: String,
    pub target_contract_digest: String,
    pub expected_keeper_pid: i64,
    pub expected_keeper_epoch: String,
    pub expected_binding_digest: String,
    pub required_action: String,
}

impl KeeperUpdateAdmissionV1 {
    pub fn parse(value: &Value) -> ProtocolResult<Self> {
        let admission: Self = decode(value, "admission")?;
        admission.validate()?;
        Ok(admission)
    }

    /// The refinements, in the order the schema ran them.
    pub fn validate(&self) -> ProtocolResult<()> {
        // An admission that classified the update as blocked is not an
        // admission at all: only the two actionable classifications are legal.
        one_of(
            "admission.classification",
            &self.classification,
            &KEEPER_UPDATE_CLASSIFICATIONS[..2],
        )?;
        hex_of_len(
            "admission.source_contract_digest",
            &self.source_contract_digest,
            SHA256_DIGEST_LENGTH,
        )?;
        hex_of_len(
            "admission.target_contract_digest",
            &self.target_contract_digest,
            SHA256_DIGEST_LENGTH,
        )?;
        integer_in_range(
            "admission.expected_keeper_pid",
            self.expected_keeper_pid,
            1,
            MAX_SAFE_INTEGER,
        )?;
        uuid(
            "admission.expected_keeper_epoch",
            &self.expected_keeper_epoch,
        )?;
        hex_of_len(
            "admission.expected_binding_digest",
            &self.expected_binding_digest,
            SHA256_DIGEST_LENGTH,
        )?;
        one_of(
            "admission.required_action",
            &self.required_action,
            &KEEPER_UPDATE_REQUIRED_ACTIONS,
        )?;
        let expected_action = if self.classification == WORKER_ONLY_SAFE {
            PRESERVE
        } else {
            REPLACE_EMPTY
        };
        if self.required_action != expected_action {
            return Err(ProtocolError::new(
                "admission.required_action",
                "keeper update classification and required action disagree",
            ));
        }
        if self.required_action == REPLACE_EMPTY
            && self.expected_binding_digest != KEEPER_EMPTY_BINDING_DIGEST
        {
            return Err(ProtocolError::new(
                "admission.expected_binding_digest",
                "replace-empty requires the canonical empty binding digest",
            ));
        }
        if self.required_action == PRESERVE
            && self.source_contract_digest != self.target_contract_digest
        {
            return Err(ProtocolError::new(
                "admission.target_contract_digest",
                "preserve requires equal source and target implementation digests",
            ));
        }
        Ok(())
    }
}

/// The immutable journal entry a deploy writes before it restarts a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct JournaledKeeperUpdateV1 {
    pub admission: KeeperUpdateAdmissionV1,
    pub source_contract: KeeperContractV1,
    pub target_contract: KeeperContractV1,
}

impl JournaledKeeperUpdateV1 {
    pub fn parse(value: &Value) -> ProtocolResult<Self> {
        let update: Self = decode(value, "journaled_update")?;
        update.validate()?;
        Ok(update)
    }

    /// A journal whose contracts disagree with its own admission is a proof of
    /// nothing, so the disagreement is refused rather than re-derived.
    pub fn validate(&self) -> ProtocolResult<()> {
        self.admission.validate()?;
        let agrees = self.source_contract.implementation_digest.as_deref()
            == Some(self.admission.source_contract_digest.as_str())
            && self.target_contract.implementation_digest.as_deref()
                == Some(self.admission.target_contract_digest.as_str());
        if !agrees {
            return Err(ProtocolError::new(
                "admission",
                "journaled keeper contract digests disagree with admission",
            ));
        }
        let same_implementation =
            keeper_contracts_same_implementation(&self.target_contract, &self.source_contract);
        let classified_preservable = self.admission.classification == WORKER_ONLY_SAFE;
        if classified_preservable != same_implementation {
            return Err(ProtocolError::new(
                "admission.classification",
                "journaled keeper contracts disagree with classification",
            ));
        }
        Ok(())
    }
}

fn validate_sorted_features(field: &str, features: &[String]) -> ProtocolResult<()> {
    if features.len() > CONTRACT_MAX_FEATURES {
        return Err(ProtocolError::new(
            field,
            format!("must not exceed {CONTRACT_MAX_FEATURES} features"),
        ));
    }
    for (index, feature) in features.iter().enumerate() {
        let path = format!("{field}[{index}]");
        non_empty(&path, feature)?;
        max_utf8_bytes(&path, feature, CONTRACT_MAX_FEATURE_LENGTH)?;
        if index > 0 && features[index - 1] >= *feature {
            return Err(ProtocolError::new(
                &path,
                "keeper contract features must be sorted and unique",
            ));
        }
    }
    Ok(())
}

fn bounded(field: &str, value: &str, max_bytes: usize) -> ProtocolResult<()> {
    non_empty(field, value)?;
    max_utf8_bytes(field, value, max_bytes)
}

/// Join an object's path and a key, so an error names the offending field.
fn field(path: &str, key: &str) -> String {
    format!("{path}.{key}")
}

/// Decode a wire object strictly.
///
/// Takes the value by clone because `serde_json` decodes by value; every caller
/// is a deploy or a restart, not a hot path.
fn decode<T: serde::de::DeserializeOwned>(value: &Value, path: &str) -> ProtocolResult<T> {
    serde_json::from_value(value.clone())
        .map_err(|error| ProtocolError::new(path, error.to_string()))
}
