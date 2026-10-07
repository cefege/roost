//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_status_retirement_proto.rs"]
mod agent_status_retirement_proto;
#[path = "attachment_transfer_packets.rs"]
mod attachment_transfer_packets;
#[path = "branded_ids.rs"]
mod branded_ids;
#[path = "cell_delta_admission.rs"]
mod cell_delta_admission;
#[path = "cell_delta_batch.rs"]
mod cell_delta_batch;
#[path = "cell_frame_proto.rs"]
mod cell_frame_proto;
#[path = "cell_geometry.rs"]
mod cell_geometry;
#[path = "cell_grid_chunk_limits.rs"]
mod cell_grid_chunk_limits;
#[path = "cell_grid_chunk_planning.rs"]
mod cell_grid_chunk_planning;
#[path = "cell_grid_chunks.rs"]
mod cell_grid_chunks;
#[path = "conformance.rs"]
mod conformance;
#[path = "control_frame_bounds.rs"]
mod control_frame_bounds;
#[path = "control_frames.rs"]
mod control_frames;
#[path = "coord_worker_proto.rs"]
mod coord_worker_proto;
#[path = "coord_worker_proto_fields.rs"]
mod coord_worker_proto_fields;
#[path = "event_proto_contract.rs"]
mod event_proto_contract;
#[path = "event_proto_failures.rs"]
mod event_proto_failures;
#[path = "event_proto_precision.rs"]
mod event_proto_precision;
#[path = "keeper_update.rs"]
mod keeper_update;
#[path = "keeper_update_shapes.rs"]
mod keeper_update_shapes;
#[path = "layout_document.rs"]
mod layout_document;
#[path = "layout_document_graph.rs"]
mod layout_document_graph;
#[path = "layout_document_proto.rs"]
mod layout_document_proto;
#[path = "layout_document_proto_bounds.rs"]
mod layout_document_proto_bounds;
#[path = "proto_adapters_contract.rs"]
mod proto_adapters_contract;
#[path = "session_event_parse.rs"]
mod session_event_parse;
#[path = "session_fold.rs"]
mod session_fold;
#[path = "session_fold_properties.rs"]
mod session_fold_properties;
#[path = "terminal_capture_command.rs"]
mod terminal_capture_command;
#[path = "terminal_capture_envelope.rs"]
mod terminal_capture_envelope;
#[path = "terminal_capture_validate.rs"]
mod terminal_capture_validate;
#[path = "terminal_capture_view.rs"]
mod terminal_capture_view;
#[path = "terminal_peer_packets.rs"]
mod terminal_peer_packets;
#[path = "terminal_peer_queue.rs"]
mod terminal_peer_queue;
#[path = "terminal_peer_sdp.rs"]
mod terminal_peer_sdp;
#[path = "terminal_peer_stun.rs"]
mod terminal_peer_stun;
#[path = "worker_registry.rs"]
mod worker_registry;
