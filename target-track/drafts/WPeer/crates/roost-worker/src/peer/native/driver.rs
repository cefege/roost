//! The task that runs one answered peer: reads its sockets, feeds str0m,
//! sends what str0m transmits from the socket of the matching local address,
//! and wakes on str0m's own deadline or on a send. Spawned by
//! `peer::native::peer_handle` once the answer is written; ends when the peer
//! closes or fails. Stands in for node-datachannel's native thread under v2
//! `apps/worker/src/terminal/peer/terminal-peer-native.ts`.

use std::sync::Arc;

use str0m::net::Transmit;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::str0m_peer::{Datagram, PeerShared};

/// Datagrams read ahead of the driver, per peer. A full queue stalls the
/// readers, and the kernel's socket buffer absorbs the rest.
const RECEIVE_QUEUE: usize = 256;
/// Larger than any datagram a WebRTC peer sends (str0m's MTU range tops out
/// well below it).
const RECEIVE_BUFFER_BYTES: usize = 2048;

pub(super) fn spawn(shared: Arc<PeerShared>, sockets: Vec<Arc<UdpSocket>>) {
    tokio::spawn(run(shared, sockets));
}

async fn run(shared: Arc<PeerShared>, sockets: Vec<Arc<UdpSocket>>) {
    let (datagrams_in, mut datagrams) = mpsc::channel(RECEIVE_QUEUE);
    let mut readers = JoinSet::new();
    for socket in &sockets {
        readers.spawn(read_socket(Arc::clone(socket), datagrams_in.clone()));
    }
    drop(datagrams_in);
    tracing::debug!(peer = %shared.config.name, sockets = sockets.len(), "a native peer driver started");
    let mut received: Option<Datagram> = None;
    loop {
        let step = shared.drive(received.take().as_ref(), std::time::Instant::now());
        for transmit in step.transmits {
            send_transmit(&sockets, transmit).await;
        }
        if !step.alive {
            break;
        }
        tokio::select! {
            datagram = datagrams.recv() => match datagram {
                Some(datagram) => received = Some(datagram),
                None => shared.fail_transport("every peer socket stopped reading"),
            },
            () = tokio::time::sleep_until(step.deadline.into()) => {}
            () = shared.wake.notified() => {}
        }
    }
    readers.abort_all();
    tracing::debug!(peer = %shared.config.name, "a native peer driver stopped");
}

async fn read_socket(socket: Arc<UdpSocket>, datagrams: mpsc::Sender<Datagram>) {
    let Ok(destination) = socket.local_addr() else {
        return;
    };
    let mut buffer = vec![0u8; RECEIVE_BUFFER_BYTES];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((length, source)) => {
                let datagram = Datagram {
                    source,
                    destination,
                    bytes: buffer[..length].to_vec(),
                };
                if datagrams.send(datagram).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                tracing::debug!(%error, %destination, "a peer socket stopped reading");
                return;
            }
        }
    }
}

/// From the socket bound to the transmit's source address: that is the
/// candidate the remote paired with.
async fn send_transmit(sockets: &[Arc<UdpSocket>], transmit: Transmit) {
    let socket = sockets
        .iter()
        .find(|socket| socket.local_addr().is_ok_and(|local| local == transmit.source))
        .or_else(|| {
            sockets.iter().find(|socket| {
                socket
                    .local_addr()
                    .is_ok_and(|local| local.is_ipv4() == transmit.destination.is_ipv4())
            })
        });
    let Some(socket) = socket else {
        tracing::debug!(destination = %transmit.destination, "no peer socket can reach a transmit's destination");
        return;
    };
    if let Err(error) = socket.send_to(&transmit.contents, transmit.destination).await {
        tracing::debug!(%error, destination = %transmit.destination, "a peer datagram could not be sent");
    }
}
