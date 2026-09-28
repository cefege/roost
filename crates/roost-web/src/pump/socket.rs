//! The one live Sync socket: open it for a dial, drain what it observed into
//! the core, write the core's commands to it, close it on request.
//!
//! Called by `pump::effects` (`DialSync`, `SendSync`, `CloseSyncLink`) and by
//! the socket's own notify. Depends on `platform::sync_socket` and the client
//! core's `sync::decode` and `client::sync::encode`. Ported from the socket
//! half of `apps/web/src/store/sync.ts:171-265` and
//! `apps/web/src/store/sync-inbound.ts:52-85` (`_consumeSyncFrame`).

use std::rc::Rc;

use roost_client_core::client::sync::encode::encode_sync_command;
use roost_client_core::sync::decode::{SyncFrameMeta, decode_firehose};
use roost_client_core::{ClientEvent, SyncCommand, SyncDial, SyncFrame};

use super::Pump;
use crate::platform::sync_socket::{
    SyncSocket, SyncSocketHandle, SyncSocketMessage, WebSocketSyncSocket,
};

/// The socket a generation owns.
#[derive(Debug)]
pub(super) struct LiveSocket {
    generation: u64,
    handle: SyncSocketHandle,
}

/// Open the socket for `generation`. The bearer is minted first, so the dial
/// is asynchronous; a dial that cannot open still retires its generation.
pub(super) fn open(pump: &Pump, generation: u64, dial: SyncDial) {
    let pump = pump.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let bearer = pump.inner.rpc.bearer().await;
        let notify: Rc<dyn Fn()> = {
            let pump = pump.clone();
            Rc::new(move || schedule_drain(&pump))
        };
        let transport = WebSocketSyncSocket::new(pump.inner.rpc.base_url());
        match transport.open(&dial, bearer, notify) {
            Ok(handle) => {
                tracing::info!(target: "sync", generation, "sync socket dialling");
                // Replacing the previous handle drops it, and a dropped handle
                // closes its socket as a retired generation.
                *pump.inner.socket.borrow_mut() = Some(LiveSocket { generation, handle });
            }
            Err(error) => {
                tracing::warn!(target: "sync", generation, %error, "sync socket could not open");
                pump.dispatch(ClientEvent::SyncLinkClosed {
                    generation,
                    close_code: None,
                });
            }
        }
    });
}

/// Drain on the next task, never inside the callback that queued the message.
fn schedule_drain(pump: &Pump) {
    let pump = pump.clone();
    wasm_bindgen_futures::spawn_local(async move { drain(&pump) });
}

/// Hand every queued observation to the core, in arrival order.
fn drain(pump: &Pump) {
    let (generation, messages, accepting) = {
        let socket = pump.inner.socket.borrow();
        let Some(live) = socket.as_ref() else { return };
        (
            live.generation,
            live.handle.drain(),
            live.handle.is_accepting(),
        )
    };
    for message in messages {
        match message {
            // OPEN only proves the upgrade; `subscribed` is what opens the link.
            SyncSocketMessage::Open => {
                tracing::debug!(target: "sync", generation, "sync socket open");
            }
            SyncSocketMessage::Binary(bytes) => {
                if accepting {
                    deliver(pump, generation, &bytes);
                }
            }
            SyncSocketMessage::Closed { code, reason } => {
                tracing::info!(target: "sync", generation, code, %reason, "sync socket closed");
                let mut socket = pump.inner.socket.borrow_mut();
                if socket
                    .as_ref()
                    .is_some_and(|live| live.generation == generation)
                {
                    *socket = None;
                }
                drop(socket);
                pump.dispatch(ClientEvent::SyncLinkClosed {
                    generation,
                    close_code: Some(code),
                });
            }
        }
    }
}

/// Decode one binary message and hand it to the core. The `subscribed`
/// announcement opens the link first: its socket id and epoch exist nowhere
/// else (v2 `handleSubscribed` builds `link.v2` from them).
fn deliver(pump: &Pump, generation: u64, bytes: &[u8]) {
    match decode_firehose(bytes, SyncFrameMeta { generation }) {
        Ok(event) => {
            if let ClientEvent::SyncFrameReceived {
                frame:
                    SyncFrame::Subscribed {
                        socket_id,
                        process_epoch,
                        ..
                    },
                ..
            } = &event
            {
                pump.dispatch(ClientEvent::SyncLinkOpened {
                    generation,
                    socket_id: socket_id.clone(),
                    process_epoch: process_epoch.clone(),
                });
            }
            pump.dispatch(event);
        }
        Err(refusal) => {
            tracing::warn!(target: "sync", generation, %refusal, "sync frame refused");
            pump.dispatch(ClientEvent::SyncFrameRefused {
                generation,
                reason: refusal.to_string(),
            });
        }
    }
}

/// Write one command on the live socket, stamped with its socket id.
pub(super) fn send(pump: &Pump, command: &SyncCommand) {
    let Some(socket_id) = pump
        .inner
        .core
        .borrow()
        .store()
        .sync
        .socket_id()
        .map(str::to_owned)
    else {
        tracing::debug!(target: "sync", "no open link; command not sent");
        return;
    };
    let bytes = encode_sync_command(command, &socket_id);
    let socket = pump.inner.socket.borrow();
    let sent = socket.as_ref().is_some_and(|live| live.handle.send(&bytes));
    if !sent {
        tracing::warn!(target: "sync", "sync command not sent: the socket is not open");
    }
}

/// Close the socket `generation` owns, so the redial loop replaces it.
pub(super) fn close(pump: &Pump, generation: u64, reason: &str) {
    let socket = pump.inner.socket.borrow();
    if let Some(live) = socket.as_ref().filter(|live| live.generation == generation) {
        tracing::info!(target: "sync", generation, reason, "closing sync socket");
        live.handle.close(1000, reason);
    }
}
