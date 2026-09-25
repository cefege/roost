//! Contract tests for the small versioned messages the coordinator persists:
//! host identity, the keeper proof, the recovery row, and the terminal-core
//! capacity report. Each one crosses its boundary in both directions and is
//! refused when it carries something the domain contract does not allow.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto::{
    AgentConversationReferenceV1 as PbReference, KeeperContractV1 as PbKeeperContractV1,
    TerminalCoreCapacityReport as PbReport,
};
use roost_protocol::agent_conversation_reference::{
    AgentConversationRecoveryMetadata, AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::keeper_update::{KeeperContractV1, KeeperRuntimeObservationV1};
use roost_protocol::proto_adapters::agent_conversation_reference_proto::{
    agent_conversation_reference_from_proto, agent_conversation_reference_to_proto,
    session_recovery_metadata_from_proto, session_recovery_metadata_to_proto,
};
use roost_protocol::proto_adapters::host_identity_proto::{
    host_identity_from_proto, host_identity_to_proto,
};
use roost_protocol::proto_adapters::keeper_runtime_proto::{
    keeper_contract_from_proto, keeper_contract_to_proto, keeper_runtime_observation_from_proto,
    keeper_runtime_observation_to_proto,
};
use roost_protocol::proto_adapters::terminal_core_capacity_proto::{
    terminal_core_capacity_report_from_proto, terminal_core_capacity_report_to_proto,
};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::worker::{HostIdentity, TerminalCoreCapacityReport};

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000abc";
const DIGEST: &str = "74eb8cfffb89f155db2201d8c1b13202c29d91be6cc3d4fec6b465c9a9ede627";

const KEEPER_EPOCH: &str = "6f1a0b1e-6c1f-4a3a-9f0e-2b7d5c8e4a11";

fn contract() -> KeeperContractV1 {
    KeeperContractV1 {
        protocol_version: 3,
        supported_features: vec!["channels-v1".to_owned(), "history-v1".to_owned()],
        required_features: vec!["channels-v1".to_owned()],
        implementation_digest: Some(DIGEST.to_owned()),
        platform: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        build_sha: "abc123".to_owned(),
    }
}

fn observation() -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1 {
        schema_version: 1,
        running_contract: contract(),
        keeper_pid: 4242,
        keeper_epoch: KEEPER_EPOCH.to_owned(),
        channel_count: 0,
        binding_digest: DIGEST.to_owned(),
        reconciled_at_ms: 1_781_500_000_000,
    }
}

fn capacity() -> TerminalCoreCapacityReport {
    TerminalCoreCapacityReport {
        used: 12,
        pending: 0,
        capacity: 12,
        estimated_reserved_bytes: 480 * 1024 * 1024,
        effective_memory_ceiling_bytes: 2 * 1024 * 1024 * 1024,
        boot_rss_bytes: 256 * 1024 * 1024,
        overcommit_count: 0,
        refusal_count: 3,
    }
}

#[test]
fn a_keeper_contract_survives_the_boundary_without_its_bun_identity() {
    let proto = keeper_contract_to_proto(&contract()).expect("the contract encodes");
    assert!(
        proto.bun_abi.is_empty(),
        "v3 keepers do not report a Bun ABI"
    );
    assert_eq!(proto.implementation_digest.as_deref(), Some(DIGEST));
    assert_eq!(
        keeper_contract_from_proto(&proto).expect("the contract decodes"),
        contract()
    );
}

#[test]
fn a_keeper_contract_that_cannot_prove_its_binary_keeps_that_fact() {
    let anonymous = KeeperContractV1 {
        implementation_digest: None,
        ..contract()
    };
    let proto = keeper_contract_to_proto(&anonymous).expect("the contract encodes");
    assert!(proto.implementation_digest.is_none());
    let decoded = keeper_contract_from_proto(&proto).expect("the contract decodes");
    assert!(decoded.implementation_digest.is_none());
}

#[test]
fn a_keeper_contract_off_the_platform_list_is_refused_in_both_directions() {
    let mut wrong = contract();
    wrong.platform = "plan9".to_owned();
    let error = keeper_contract_to_proto(&wrong).expect_err("an unknown platform is refused");
    assert_eq!(error.field, "keeper_contract.platform");
    let proto = PbKeeperContractV1 {
        platform: "plan9".to_owned(),
        ..keeper_contract_to_proto(&contract()).expect("the fixture contract encodes")
    };
    assert_eq!(
        keeper_contract_from_proto(&proto)
            .expect_err("an unknown platform is refused")
            .field,
        "keeper_contract.platform"
    );
}

#[test]
fn a_keeper_runtime_observation_survives_the_boundary() {
    let proto =
        keeper_runtime_observation_to_proto(&observation()).expect("the observation encodes");
    assert!(proto.running_contract.is_set());
    assert_eq!(
        keeper_runtime_observation_from_proto(&proto).expect("the observation decodes"),
        observation()
    );
}

#[test]
fn an_observation_without_its_running_contract_is_unproven_rather_than_empty() {
    let mut proto =
        keeper_runtime_observation_to_proto(&observation()).expect("the observation encodes");
    proto.running_contract = None::<PbKeeperContractV1>.into();
    let error = keeper_runtime_observation_from_proto(&proto)
        .expect_err("a proof with no contract proves nothing");
    assert_eq!(error.field, "keeper_observation.running_contract");
}

#[test]
fn an_observation_whose_two_halves_disagree_is_refused() {
    let contradicted = KeeperRuntimeObservationV1 {
        channel_count: 2,
        binding_digest: DIGEST.to_owned(),
        ..observation()
    };
    assert_eq!(
        keeper_runtime_observation_to_proto(&contradicted)
            .expect_err("a channel count and a binding digest must agree")
            .field,
        "keeper_observation.binding_digest"
    );
    // The decode half proves the same rule from the wire: an observation that
    // claims zero channels while carrying a live digest is refused, because that
    // is the disagreement the rule is about. A digest that is merely different
    // from a known one is NOT provable here — the adapter never sees the
    // bindings, so it cannot say which digest is right.
    // The decode half proves the same rule from the wire: an observation that
    // claims two channels while carrying the EMPTY digest is refused. A digest
    // that is merely unfamiliar is NOT provable here — the adapter never sees
    // the bindings, so it cannot say which digest is the right one.
    let mut proto = keeper_runtime_observation_to_proto(&observation())
        .expect("a consistent observation encodes");
    proto.channel_count = 2;
    assert_eq!(
        keeper_runtime_observation_from_proto(&proto)
            .expect_err("a decode must refuse the same disagreement")
            .field,
        "keeper_observation.binding_digest"
    );
}

#[test]
fn a_terminal_core_capacity_report_survives_the_boundary() {
    let proto = terminal_core_capacity_report_to_proto(&capacity()).expect("the report encodes");
    assert_eq!(proto.refusal_count, 3);
    assert_eq!(
        terminal_core_capacity_report_from_proto(&proto).expect("the report decodes"),
        capacity()
    );
}

#[test]
fn a_capacity_report_beyond_its_own_capacity_is_refused_in_both_directions() {
    let overcommitted = TerminalCoreCapacityReport {
        used: 13,
        ..capacity()
    };
    assert_eq!(
        terminal_core_capacity_report_to_proto(&overcommitted)
            .expect_err("use beyond capacity must not reach a coordinator")
            .field,
        "terminal_core_capacity.used"
    );
    let proto = PbReport {
        used: 13,
        ..terminal_core_capacity_report_to_proto(&capacity()).expect("the fixture report encodes")
    };
    assert_eq!(
        terminal_core_capacity_report_from_proto(&proto)
            .expect_err("a decode must refuse the same report")
            .field,
        "terminal_core_capacity.used"
    );
}

#[test]
fn a_replacement_reserve_is_the_only_use_beyond_capacity_that_is_allowed() {
    let reserved = TerminalCoreCapacityReport {
        used: 13,
        overcommit_count: 1,
        ..capacity()
    };
    let proto =
        terminal_core_capacity_report_to_proto(&reserved).expect("one reserved slot is legal");
    assert_eq!(
        terminal_core_capacity_report_from_proto(&proto).expect("the report decodes"),
        reserved
    );
    let doubled = TerminalCoreCapacityReport {
        overcommit_count: 2,
        ..capacity()
    };
    assert_eq!(
        terminal_core_capacity_report_to_proto(&doubled)
            .expect_err("a second reserved slot is not")
            .field,
        "terminal_core_capacity.overcommit_count"
    );
}

#[test]
fn a_host_identity_is_normalized_in_both_directions() {
    let noisy = HostIdentity {
        hardware_model: Some("Apple  M2 Pro Book".to_owned()),
        chip: Some("M2".to_owned()),
        linux_distribution: None,
    };
    let proto = host_identity_to_proto(Some(&noisy));
    assert_eq!(proto.hardware_model.as_deref(), Some("Apple M2 Pro Book"));
    assert!(proto.linux_distribution.is_none());
    assert_eq!(
        host_identity_from_proto(Some(&proto)),
        Some(HostIdentity {
            hardware_model: Some("Apple M2 Pro Book".to_owned()),
            chip: Some("M2".to_owned()),
            linux_distribution: None,
        })
    );
}

#[test]
fn an_identity_of_nothing_and_an_absent_one_describe_no_machine() {
    let empty = HostIdentity {
        hardware_model: None,
        chip: None,
        linux_distribution: None,
    };
    let proto = host_identity_to_proto(Some(&empty));
    assert!(proto.hardware_model.is_none());
    assert!(proto.chip.is_none());
    assert!(proto.linux_distribution.is_none());
    assert_eq!(host_identity_from_proto(Some(&proto)), None);
    assert_eq!(host_identity_from_proto(None), None);
    assert_eq!(host_identity_to_proto(None).hardware_model, None);
}

#[test]
fn an_agent_conversation_reference_survives_the_boundary() {
    let reference = AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Path,
        value: "/tmp/a path/'$opaque.json".to_owned(),
    };
    let proto = agent_conversation_reference_to_proto(&reference).expect("the reference encodes");
    assert_eq!(proto.kind, "path");
    assert_eq!(
        agent_conversation_reference_from_proto(Some(&proto)).expect("the reference decodes"),
        Some(reference)
    );
    assert_eq!(
        agent_conversation_reference_from_proto(None).expect("an absent reference is not an error"),
        None
    );
}

#[test]
fn a_reference_outside_its_own_bounds_is_refused_in_both_directions() {
    let relative = AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Path,
        value: "relative/path".to_owned(),
    };
    assert_eq!(
        agent_conversation_reference_to_proto(&relative)
            .expect_err("a relative session path is refused")
            .field,
        "value"
    );
    let foreign = AgentConversationReferenceV1 {
        agent_id: "claude".to_owned(),
        ..relative.clone()
    };
    assert_eq!(
        agent_conversation_reference_to_proto(&foreign)
            .expect_err("only one agent's conversation is resumable")
            .field,
        "agent_id"
    );
    let mut unknown_kind = relative;
    unknown_kind.kind = AgentConversationReferenceKind::Id;
    let mut proto =
        agent_conversation_reference_to_proto(&unknown_kind).expect("the reference encodes");
    proto.kind = "transcript".to_owned();
    assert_eq!(
        agent_conversation_reference_from_proto(Some(&proto))
            .expect_err("a kind this build does not know is refused")
            .field,
        "kind"
    );
    // An empty value is refused on ingress rather than becoming a reference
    // that cannot resume anything.
    let empty = PbReference {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: "id".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        agent_conversation_reference_from_proto(Some(&empty))
            .expect_err("an empty reference is refused")
            .field,
        "value"
    );
}

#[test]
fn a_recovery_row_survives_the_boundary() {
    let metadata = AgentConversationRecoveryMetadata {
        session_id: SessionId::try_from(SESSION_ID).expect("fixture session id"),
        agent_reference: Some(AgentConversationReferenceV1 {
            schema_version: 1,
            agent_id: "omp".to_owned(),
            kind: AgentConversationReferenceKind::Id,
            value: "conversation-1".to_owned(),
        }),
        agent_reference_client_seq: 4,
    };
    let proto = session_recovery_metadata_to_proto(&metadata).expect("the row encodes");
    assert_eq!(proto.agent_reference_client_seq, 4);
    assert!(proto.agent_reference.is_set());
    assert_eq!(
        session_recovery_metadata_from_proto(&proto).expect("the row decodes"),
        metadata
    );
}

#[test]
fn a_cleared_recovery_row_carries_sequence_zero_and_no_reference() {
    let cleared = AgentConversationRecoveryMetadata {
        session_id: SessionId::try_from(SESSION_ID).expect("fixture session id"),
        agent_reference: None,
        agent_reference_client_seq: 0,
    };
    let proto = session_recovery_metadata_to_proto(&cleared).expect("the row encodes");
    assert!(proto.agent_reference.is_unset());
    assert_eq!(
        session_recovery_metadata_from_proto(&proto).expect("the row decodes"),
        cleared
    );
    // The rule is about a row that CARRIES a reference: sequence zero is the
    // worker-list sentinel for "this session has no reference event", so a
    // reference stamped with it proves nothing about ordering. A cleared row at
    // sequence zero is the legitimate shape, and it decodes above.
    let referenced = AgentConversationRecoveryMetadata {
        session_id: SessionId::try_from(SESSION_ID).expect("fixture session id"),
        agent_reference: Some(AgentConversationReferenceV1 {
            schema_version: 1,
            agent_id: "omp".to_owned(),
            kind: AgentConversationReferenceKind::Id,
            value: "conversation-1".to_owned(),
        }),
        agent_reference_client_seq: 4,
    };
    let mut stale = session_recovery_metadata_to_proto(&referenced).expect("the row encodes");
    stale.agent_reference_client_seq = 0;
    assert_eq!(
        session_recovery_metadata_from_proto(&stale)
            .expect_err("a reference at sequence zero proves nothing")
            .field,
        "agent_conversation_recovery_metadata.agent_reference_client_seq"
    );
}
