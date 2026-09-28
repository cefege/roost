//! Hello admission for one direct port: the peer tuple (or its absence on
//! loopback) is checked, the credential verified against the grant store, the
//! worker epoch fenced, a loopback grant's prior socket replaced, and only then
//! is the port bound, told it is ready, and registered with the terminal view
//! owner under the grant's live session scope. Called by `super::sockets` for a
//! `hello` frame. Ports `accept`/`matchesExpectedPeer` of
//! `apps/worker/src/local-door/local-terminal-socket.ts`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::{LocalTerminalHello, LocalTerminalReady};

use super::authority::{Carrier, PortIdentity, PortSession};
use super::delivery::PortViewTransport;
use super::grant_scope::GrantCredential;
use super::sockets::LocalTerminalSockets;
use crate::terminal_view::{LocalViewRegistration, LocalViewTransport};

impl LocalTerminalSockets {
    pub(super) fn accept(&self, session: &Arc<PortSession>, hello: &LocalTerminalHello) {
        if session.is_authenticated() {
            self.close(session, "duplicate hello");
            return;
        }
        if !matches_expected_peer(session, hello) {
            let reason = if session.expected_peer().is_some() {
                "peer hello does not match offer"
            } else {
                "loopback hello must not include peer identity"
            };
            self.close(session, reason);
            return;
        }
        let credential = GrantCredential {
            grant_id: &hello.grant_id,
            secret: &hello.secret,
            tab_id: &hello.tab_id,
            device_fingerprint: &hello.device_fingerprint,
        };
        let grant = match self.authorization.grants.verify(credential) {
            Ok(grant) => grant,
            Err(reason) => {
                tracing::warn!(socket_id = session.socket_id(), grant_id = %hello.grant_id, reason, "local terminal hello refused");
                self.close(session, reason);
                return;
            }
        };
        let worker_epoch = self.authorization.worker_epoch.as_str();
        if let Some(expected) = session.expected_peer() {
            if grant.worker_epoch != expected.worker_epoch {
                self.close(session, "peer grant is unavailable");
                return;
            }
        } else if !grant.worker_epoch.is_empty() && grant.worker_epoch != worker_epoch {
            self.close(session, "local terminal worker epoch changed");
            return;
        }
        if session.expected_peer().is_none() {
            let admission = self.pre_hello.authenticate(&grant.grant_id, session.socket_id());
            if !admission.admitted {
                self.close(session, "local terminal authenticated capacity reached");
                return;
            }
            let replaced = admission.replaced_socket_id.and_then(|socket_id| self.authorization.ports.get(&socket_id));
            if let Some(replaced) = replaced {
                self.close(&replaced, "local terminal grant connection replaced");
            }
        }
        let generation = self.generations.fetch_add(1, Ordering::AcqRel) + 1;
        session.bind(PortIdentity {
            generation,
            grant_id: Some(grant.grant_id.clone()),
            device_fingerprint: Some(grant.device_fingerprint.clone()),
            tab_id: Some(grant.tab_id.clone()),
        });
        if let Carrier::Peer { port, .. } = &session.carrier {
            port.mark_authenticated();
        }
        let ready = LocalTerminalReady {
            worker_fingerprint: self.worker_fingerprint.clone(),
            session_ids: grant.session_ids.clone(),
            socket_generation: generation,
            worker_epoch: worker_epoch.to_owned(),
            socket_id: session.socket_id().to_owned(),
            peer_id: session.expected_peer().map(|expected| expected.peer_id.clone()).unwrap_or_default(),
            ..Default::default()
        };
        if !self.send_control(session, ServerFrame::from(ready)) {
            return;
        }
        let scope_owner = self.self_handle.clone();
        let scope_session = Arc::clone(session);
        self.view.register_local(LocalViewRegistration {
            socket_id: session.socket_id().to_owned(),
            device_fingerprint: grant.device_fingerprint.clone(),
            tab_id: grant.tab_id.clone(),
            allows_session: Arc::new(move |session_id: &str| {
                scope_owner
                    .upgrade()
                    .is_some_and(|sockets| sockets.is_session_authorized(&scope_session, session_id))
            }),
            transport: Arc::new(PortViewTransport { sockets: self.self_handle.clone(), session: Arc::clone(session) })
                as Arc<dyn LocalViewTransport>,
        });
        tracing::info!(
            socket_id = session.socket_id(),
            grant_id = %grant.grant_id,
            device_fingerprint = %grant.device_fingerprint,
            tab_id = %grant.tab_id,
            socket_generation = generation,
            sessions = grant.session_ids.len(),
            kind = session.kind(),
            "local terminal hello accepted"
        );
    }
}

/// A peer Hello repeats its offer tuple exactly; a loopback Hello names no
/// peer and no epoch.
fn matches_expected_peer(session: &PortSession, hello: &LocalTerminalHello) -> bool {
    let Some(expected) = session.expected_peer() else {
        return hello.peer_id.is_empty() && hello.worker_epoch.is_empty();
    };
    hello.peer_id == expected.peer_id
        && hello.grant_id == expected.grant_id
        && hello.device_fingerprint == expected.device_fingerprint
        && hello.tab_id == expected.tab_id
        && hello.worker_epoch == expected.worker_epoch
}
