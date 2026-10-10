#![cfg(unix)]

use std::time::Duration;

use roost_proto::{DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen};
use roost_protocol::wire::coord_worker::{AgentTunnelState, CoordWorkerUpstream};
use roost_worker::link_ports::AgentTunnelPort;
use roost_worker::runtime::agent_tunnel::{AgentTunnelOwner, AgentTunnelQueue};
use roost_worker::uplink;
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "roost-agent-tunnel-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn next_frame(receiver: &mut uplink::UplinkReceiver) -> CoordWorkerUpstream {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

fn native_platform() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        }
    )
}

#[tokio::test]
async fn uploads_verified_daemon_and_round_trips_child_stdin_and_stdout() {
    let cache = temp_dir();
    let daemon = b"#!/bin/sh\nIFS= read -r value\nprintf 'reply:%s\\n' \"$value\" | cat\n";
    let (uplink, mut receiver) = uplink::channel();
    let owner = AgentTunnelOwner::new(cache.clone(), uplink);
    let platform = native_platform();
    owner
        .open(DAgentTunnelOpen {
            tunnel_id: "roundtrip".into(),
            args: vec![
                "serve".into(),
                "--token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ],
            daemon_sha256: std::iter::once((platform.clone(), sha(daemon))).collect(),
            ..Default::default()
        })
        .await;
    assert!(
        matches!(next_frame(&mut receiver).await, CoordWorkerUpstream::AgentTunnelState(frame) if frame.state == AgentTunnelState::NeedDaemon)
    );
    owner
        .daemon_chunk(DAgentTunnelDaemonChunk {
            tunnel_id: "roundtrip".into(),
            data: daemon.to_vec(),
            last: true,
            ..Default::default()
        })
        .await;
    assert!(
        matches!(next_frame(&mut receiver).await, CoordWorkerUpstream::AgentTunnelState(frame) if frame.state == AgentTunnelState::Opened)
    );
    owner
        .input(roost_proto::DAgentTunnelInput {
            tunnel_id: "roundtrip".into(),
            data: b"ping\n".to_vec(),
            ..Default::default()
        })
        .await;
    let mut output = Vec::new();
    loop {
        match next_frame(&mut receiver).await {
            CoordWorkerUpstream::AgentTunnelOutput(frame) if !frame.stderr => {
                output.extend(frame.data)
            }
            CoordWorkerUpstream::AgentTunnelState(frame)
                if frame.state == AgentTunnelState::Closed =>
            {
                break;
            }
            _ => {}
        }
    }
    assert_eq!(output, b"reply:ping\n");
    let _ = tokio::fs::remove_dir_all(cache).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_port_applies_back_to_back_frames_in_link_order() {
    let cache = temp_dir();
    let daemon = b"#!/bin/sh\nexec cat\n";
    let (uplink, mut receiver) = uplink::channel();
    let port = AgentTunnelQueue::start(AgentTunnelOwner::new(cache.clone(), uplink));
    port.open(DAgentTunnelOpen {
        tunnel_id: "ordered".into(),
        args: vec![
            "serve".into(),
            "--token".into(),
            "0123456789abcdef0123456789abcdef".into(),
        ],
        daemon_sha256: std::iter::once((native_platform(), sha(daemon))).collect(),
        ..Default::default()
    });
    // One byte per chunk: any reordering changes the digest.
    for byte in daemon {
        port.daemon_chunk(DAgentTunnelDaemonChunk {
            tunnel_id: "ordered".into(),
            data: vec![*byte],
            last: false,
            ..Default::default()
        });
    }
    port.daemon_chunk(DAgentTunnelDaemonChunk {
        tunnel_id: "ordered".into(),
        data: Vec::new(),
        last: true,
        ..Default::default()
    });
    let mut expected = Vec::new();
    for line in 0..64 {
        let data = format!("line-{line}\n").into_bytes();
        expected.extend_from_slice(&data);
        port.input(DAgentTunnelInput {
            tunnel_id: "ordered".into(),
            data,
            ..Default::default()
        });
    }
    let mut output = Vec::new();
    while output.len() < expected.len() {
        match next_frame(&mut receiver).await {
            CoordWorkerUpstream::AgentTunnelOutput(frame) if !frame.stderr => {
                output.extend(frame.data)
            }
            CoordWorkerUpstream::AgentTunnelState(frame)
                if frame.state == AgentTunnelState::Closed =>
            {
                panic!("tunnel closed early: {}", frame.error);
            }
            _ => {}
        }
    }
    assert_eq!(output, expected);
    let _ = tokio::fs::remove_dir_all(cache).await;
}

#[tokio::test]
async fn rejects_uploaded_daemon_with_a_sha_mismatch() {
    let cache = temp_dir();
    let (uplink, mut receiver) = uplink::channel();
    let owner = AgentTunnelOwner::new(cache.clone(), uplink);
    let platform = format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        }
    );
    owner
        .open(DAgentTunnelOpen {
            tunnel_id: "mismatch".into(),
            args: vec![
                "serve".into(),
                "--token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ],
            daemon_sha256: std::iter::once((platform, sha(b"expected daemon"))).collect(),
            ..Default::default()
        })
        .await;
    let _ = next_frame(&mut receiver).await;
    owner
        .daemon_chunk(DAgentTunnelDaemonChunk {
            tunnel_id: "mismatch".into(),
            data: b"not the daemon".to_vec(),
            last: true,
            ..Default::default()
        })
        .await;
    assert!(
        matches!(next_frame(&mut receiver).await, CoordWorkerUpstream::AgentTunnelState(frame) if frame.state == AgentTunnelState::Closed && frame.error == "daemon SHA mismatch")
    );
    let _ = tokio::fs::remove_dir_all(cache).await;
}

#[tokio::test]
async fn reports_unsupported_platform_when_manifest_has_no_native_daemon() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = AgentTunnelOwner::new(temp_dir(), uplink);
    owner
        .open(DAgentTunnelOpen {
            tunnel_id: "unsupported".into(),
            args: vec![
                "serve".into(),
                "--token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ],
            daemon_sha256: Default::default(),
            ..Default::default()
        })
        .await;
    assert!(
        matches!(next_frame(&mut receiver).await, CoordWorkerUpstream::AgentTunnelState(frame) if frame.state == AgentTunnelState::Closed && frame.error == "unsupported platform")
    );
}
