//! Conversions between the small versioned messages the contract defines by
//! hand and their generated protobuf forms. Each is a thin mapping; none of
//! them owns behaviour, and none of them may become a second definition of a
//! value that already exists in the domain module it adapts.

pub mod agent_conversation_reference_proto;
pub mod coord_worker_proto;
pub mod host_identity_proto;
pub mod keeper_runtime_proto;
pub mod terminal_core_capacity_proto;

pub use agent_conversation_reference_proto::{
    agent_conversation_reference_from_proto, agent_conversation_reference_to_proto,
    session_recovery_metadata_from_proto, session_recovery_metadata_to_proto,
};
pub use coord_worker_proto::{
    decode_downstream, decode_upstream, encode_downstream, encode_upstream,
};
pub use host_identity_proto::{host_identity_from_proto, host_identity_to_proto};
pub use keeper_runtime_proto::{
    keeper_contract_from_proto, keeper_contract_to_proto, keeper_runtime_observation_from_proto,
    keeper_runtime_observation_to_proto,
};
pub use terminal_core_capacity_proto::{
    terminal_core_capacity_report_from_proto, terminal_core_capacity_report_to_proto,
};
