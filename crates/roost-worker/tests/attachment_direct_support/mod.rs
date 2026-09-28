//! The direct-receiver fixture the attachment direct tests share: a recording
//! loopback port, one installed dedicated grant with a movable clock, and an
//! operation owner rooted in a scratch directory removed on drop. Mirrors the
//! `FakeLoopbackPort`/`createFixture` helpers of v2
//! `apps/worker/tests/attachments/attachment-direct-socket.test.ts`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferAck, AttachmentTransferChunk, AttachmentTransferClientFrame,
    AttachmentTransferHello, AttachmentTransferServerFrame, DLocalAttachmentGrant,
};
use roost_protocol::attachment_transfer::{GRANT_TTL_MS, PeerChannelLane};
use roost_worker::attachments::direct_sockets::{
    AttachmentDirectSockets, AttachmentDirectSocketsDeps, DirectLane,
};
use roost_worker::attachments::grants::AttachmentGrantStore;
use roost_worker::attachments::receipts::AttachmentOperationStatus;
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::system_clock;
use roost_worker::attachments::transfer_port::{AttachmentTransferPort, PortKind, SendResult};
use roost_worker::attachments::upload::AttachmentOperations;
use roost_worker::session::ids::mint_uuid;
use sha2::{Digest, Sha256};

pub const DEVICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const WORKER_FP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A loopback port that records every frame and close it is given.
#[derive(Debug)]
pub struct FakeLoopbackPort {
    socket_id: String,
    frames: Mutex<Vec<Vec<u8>>>,
    close_reasons: Mutex<Vec<String>>,
    live: AtomicBool,
}

impl FakeLoopbackPort {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            socket_id: mint_uuid().unwrap(),
            frames: Mutex::default(),
            close_reasons: Mutex::default(),
            live: AtomicBool::new(true),
        })
    }

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

impl AttachmentTransferPort for FakeLoopbackPort {
    fn socket_id(&self) -> &str {
        &self.socket_id
    }

    fn kind(&self) -> PortKind {
        PortKind::Loopback
    }

    fn is_open(&self) -> bool {
        self.is_live()
    }

    fn send(&self, bytes: Vec<u8>, _lane: PeerChannelLane) -> SendResult {
        if !self.is_live() {
            return SendResult::Refused;
        }
        self.frames.lock().unwrap().push(bytes);
        SendResult::Accepted
    }

    fn close(&self, _code: Option<u16>, reason: &str) {
        self.live.store(false, Ordering::Release);
        self.close_reasons.lock().unwrap().push(reason.to_owned());
    }

    fn close_after_drain(&self, reason: &str) {
        self.close(Some(1000), reason);
    }

    fn mark_authenticated(&self) {}
}

/// One dedicated grant, its receiver, and the scratch directory uploads land in.
pub struct DirectFixture {
    pub session_id: String,
    pub upload_id: String,
    pub filename: String,
    pub total_bytes: u64,
    pub secret: String,
    pub grant_id: String,
    pub tab_id: String,
    pub worker_epoch: String,
    pub grants: Arc<AttachmentGrantStore>,
    pub operations: AttachmentOperations,
    pub sockets: AttachmentDirectSockets,
    pub root: PathBuf,
    clock_offset: Arc<Mutex<Duration>>,
}

impl DirectFixture {
    pub fn new(total_bytes: u64) -> Self {
        let root = std::env::temp_dir().join(format!("roost-direct-{}", mint_uuid().unwrap()));
        let worker_epoch = mint_uuid().unwrap();
        let clock_offset = Arc::new(Mutex::new(Duration::ZERO));
        let origin = Instant::now();
        let offset = Arc::clone(&clock_offset);
        let grants = Arc::new(AttachmentGrantStore::new(
            worker_epoch.clone(),
            Arc::new(move || origin + *offset.lock().unwrap()),
        ));
        let secret = hex(&Sha256::digest(mint_uuid().unwrap().as_bytes()));
        let (session_id, upload_id) = (
            format!("test-direct-{}", mint_uuid().unwrap()),
            mint_uuid().unwrap(),
        );
        let (grant_id, tab_id) = (mint_uuid().unwrap(), mint_uuid().unwrap());
        grants
            .install(&DLocalAttachmentGrant {
                request_id: mint_uuid().unwrap(),
                grant_id: grant_id.clone(),
                secret_sha256: hex(&Sha256::digest(secret.as_bytes())),
                session_id: session_id.clone(),
                upload_id: upload_id.clone(),
                filename: "payload.bin".to_owned(),
                short_path: false,
                total_bytes,
                device_fingerprint: DEVICE.to_owned(),
                tab_id: tab_id.clone(),
                ttl_ms: u32::try_from(GRANT_TTL_MS).unwrap(),
                worker_epoch: worker_epoch.clone(),
                ..Default::default()
            })
            .unwrap();
        let operations =
            AttachmentOperations::new(AttachmentBase::new(root.clone()), system_clock());
        let sockets = AttachmentDirectSockets::new(AttachmentDirectSocketsDeps {
            grants: Arc::clone(&grants),
            operations: operations.clone(),
            worker_fingerprint: WORKER_FP.to_owned(),
            worker_epoch: worker_epoch.clone(),
        });
        Self {
            session_id,
            upload_id,
            filename: "payload.bin".to_owned(),
            total_bytes,
            secret,
            grant_id,
            tab_id,
            worker_epoch,
            grants,
            operations,
            sockets,
            root,
            clock_offset,
        }
    }

    pub fn open(&self) -> Arc<FakeLoopbackPort> {
        let port = FakeLoopbackPort::new();
        self.sockets
            .open_loopback_port(Arc::clone(&port) as Arc<dyn AttachmentTransferPort>);
        port
    }

    pub fn advance_grant_clock(&self, by: Duration) {
        *self.clock_offset.lock().unwrap() += by;
    }

    pub fn session_dir(&self) -> PathBuf {
        AttachmentBase::new(self.root.clone()).session_dir(&self.session_id)
    }

    pub fn status(&self) -> AttachmentOperationStatus {
        self.operations.status(&self.session_id, &self.upload_id)
    }

    pub fn hello(&self, secret: &str) -> Vec<u8> {
        client_frame(ClientFrame::Hello(Box::new(AttachmentTransferHello {
            grant_id: self.grant_id.clone(),
            secret: secret.to_owned(),
            tab_id: self.tab_id.clone(),
            device_fingerprint: DEVICE.to_owned(),
            session_id: self.session_id.clone(),
            upload_id: self.upload_id.clone(),
            filename: self.filename.clone(),
            short_path: false,
            total_bytes: self.total_bytes,
            peer_id: String::new(),
            worker_epoch: self.worker_epoch.clone(),
            ..Default::default()
        })))
    }

    pub fn chunk(&self, seq: u32, offset: u64, data: &[u8], last: bool) -> (Vec<u8>, String) {
        let chunk_sha256 = hex(&Sha256::digest(data));
        let frame = client_frame(ClientFrame::Chunk(Box::new(AttachmentTransferChunk {
            upload_id: self.upload_id.clone(),
            seq,
            offset,
            data: data.to_vec(),
            last,
            chunk_sha256: chunk_sha256.clone(),
            ..Default::default()
        })));
        (frame, chunk_sha256)
    }

    pub fn send_hello(&self, port: &FakeLoopbackPort) {
        self.send_hello_with(port, &self.secret.clone());
    }

    pub fn send_hello_with(&self, port: &FakeLoopbackPort, secret: &str) {
        let write =
            self.sockets
                .receive_frame(port.socket_id(), DirectLane::Loopback, &self.hello(secret));
        assert!(write.is_none(), "a hello never writes");
    }

    pub async fn send_chunk(
        &self,
        port: &FakeLoopbackPort,
        seq: u32,
        offset: u64,
        data: &[u8],
        last: bool,
    ) -> String {
        let (frame, digest) = self.chunk(seq, offset, data, last);
        if let Some(write) =
            self.sockets
                .receive_frame(port.socket_id(), DirectLane::Loopback, &frame)
        {
            write.await;
        }
        digest
    }
}

impl Drop for DirectFixture {
    fn drop(&mut self) {
        self.sockets.dispose();
        self.grants.dispose();
        let _removed = std::fs::remove_dir_all(&self.root);
    }
}

pub fn client_frame(frame: ClientFrame) -> Vec<u8> {
    AttachmentTransferClientFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

pub fn decoded_frames(port: &FakeLoopbackPort) -> Vec<ServerFrame> {
    port.frames
        .lock()
        .unwrap()
        .iter()
        .filter_map(|bytes| {
            AttachmentTransferServerFrame::decode_from_slice(bytes)
                .unwrap()
                .frame
        })
        .collect()
}

pub fn frame_cases(port: &FakeLoopbackPort) -> Vec<&'static str> {
    decoded_frames(port)
        .iter()
        .map(|frame| match frame {
            ServerFrame::Ready(_) => "ready",
            ServerFrame::Ack(_) => "ack",
            ServerFrame::Closed(_) => "closed",
            ServerFrame::Status(_) => "status",
        })
        .collect()
}

pub fn acknowledgements(port: &FakeLoopbackPort) -> Vec<AttachmentTransferAck> {
    decoded_frames(port)
        .into_iter()
        .filter_map(|frame| match frame {
            ServerFrame::Ack(ack) => Some(*ack),
            _ => None,
        })
        .collect()
}

pub fn closed_reasons(port: &FakeLoopbackPort) -> Vec<String> {
    decoded_frames(port)
        .into_iter()
        .filter_map(|frame| match frame {
            ServerFrame::Closed(closed) => Some(closed.reason),
            _ => None,
        })
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
