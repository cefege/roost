//! What one minted grant dials, decided before a socket exists.
//!
//! Owned by `platform::carriers`, called by `pump::carrier_dial` after a
//! successful mint. It is deliberately target-independent: the two things that
//! can be wrong with a dial — there is no door, or the door is another worker's
//! — are decided here, where a test can reach them, and the browser I/O that
//! spends the credential happens afterwards.
//!
//! The rule this file exists to keep is that a mint is SPENT or REPORTED. A
//! credential the coordinator installed on a worker and this browser never
//! presented is a door left installed for a browser that has gone, so a plan
//! that cannot be built is a named [`DialFault`], never a silent return.
//!
//! The grant's worker is checked against the door's here rather than left to
//! `client::local::door::admit_ready`, because the two facts are both already in
//! hand: presenting worker A's credential on worker B's socket is a fault this
//! browser can name before it opens anything, and one it cannot name afterwards.

use std::fmt;

use roost_client_core::client::local::LocalTerminalGrant;
use roost_client_core::client::local::discovery::LocalWorkerDoor;
use roost_client_core::client::local::door::local_terminal_url;

use super::loopback_carrier::LoopbackConnection;

/// Everything `open_loopback_socket` needs, and nothing it has to compute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialPlan {
    /// The origin that answered with a usable bootstrap.
    pub origin: String,
    /// The full socket URL, already checked against the door contract.
    pub url: String,
    /// The credential, spent on open and nowhere else.
    pub hello: Vec<u8>,
}

/// Why a minted grant cannot be spent on a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialFault {
    /// This page has adopted no worker door, so there is nothing to dial.
    ///
    /// A working state rather than a failure — a coordinator-served page on a
    /// machine with no worker is the normal reason — and named as itself so a
    /// log line distinguishes it from a door that was found and refused.
    NoDoor {
        /// The worker the grant was minted for.
        worker_fp: String,
    },
    /// The adopted door answered for a different worker.
    ///
    /// The fatal one: this browser's own machine runs a worker, but not the one
    /// the coordinator minted for, and the two are the only candidates for a
    /// direct route to it.
    ForeignDoor {
        /// The worker the grant was minted for.
        worker_fp: String,
        /// The worker that answered at the door.
        door_worker_fp: String,
    },
    /// The door's origin is not one the door contract will dial.
    UnusableOrigin {
        /// The origin that was refused.
        origin: String,
    },
}

impl fmt::Display for DialFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDoor { worker_fp } => write!(
                formatter,
                "this page has no worker door, so the grant for {worker_fp} has nothing to spend on"
            ),
            Self::ForeignDoor {
                worker_fp,
                door_worker_fp,
            } => write!(
                formatter,
                "the local door answered for {door_worker_fp}, not the grant's {worker_fp}"
            ),
            Self::UnusableOrigin { origin } => {
                write!(
                    formatter,
                    "the worker door origin is not dialable: {origin}"
                )
            }
        }
    }
}

impl std::error::Error for DialFault {}

/// Resolve a minted grant against this page's door, or say why it cannot be.
///
/// `door` is the adopted door, which is `None` until discovery has answered.
/// The URL and the credential are both produced HERE so that the host which
/// opens the socket cannot build a different one: a second `local_terminal_url`
/// call in the host is a second answer to "which path does a direct carrier
/// speak", and the grant in hand would be spent on whichever came back.
pub fn plan(
    grant: &LocalTerminalGrant,
    door: Option<&LocalWorkerDoor>,
) -> Result<DialPlan, DialFault> {
    let worker_fp = grant.worker_fp.clone();
    let Some(door) = door else {
        return Err(DialFault::NoDoor { worker_fp });
    };
    if door.worker_fingerprint != worker_fp {
        return Err(DialFault::ForeignDoor {
            worker_fp,
            door_worker_fp: door.worker_fingerprint.clone(),
        });
    }
    let url = local_terminal_url(&door.origin).map_err(|_| DialFault::UnusableOrigin {
        origin: door.origin.clone(),
    })?;
    Ok(DialPlan {
        origin: door.origin.clone(),
        url,
        hello: LoopbackConnection::hello(grant),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeSet;

    use roost_client_core::client::local::{GrantMintAnswer, LocalTerminalGrant};

    use super::*;

    fn grant(worker: &str) -> LocalTerminalGrant {
        LocalTerminalGrant::from_answer(
            GrantMintAnswer {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                ttl_ms: 60_000,
                worker_epoch: "epoch-a".to_owned(),
                peer_supported: false,
                stun_urls: Vec::new(),
                input_route_supported: true,
            },
            worker,
            BTreeSet::from_iter(["session-a".to_owned()]),
            "tab-a",
            "device-a",
            0,
        )
        .unwrap()
    }

    fn door(origin: &str, worker: &str) -> LocalWorkerDoor {
        LocalWorkerDoor {
            origin: origin.to_owned(),
            worker_fingerprint: worker.to_owned(),
        }
    }

    #[test]
    fn a_minted_grant_dials_the_door_the_page_adopted() {
        let plan = plan(
            &grant("worker-a"),
            Some(&door("http://127.0.0.1:4114", "worker-a")),
        )
        .expect("a matching door is dialable");
        assert_eq!(plan.url, "ws://127.0.0.1:4114/ws/local-terminal");
        assert_eq!(plan.origin, "http://127.0.0.1:4114");
        assert!(
            !plan.hello.is_empty(),
            "the credential is built here, so the host cannot open a socket without one"
        );
    }

    #[test]
    fn a_page_with_no_door_reports_the_mint_as_unspent() {
        // The case the mint path exists for: a coordinator-served page on a
        // machine with no worker mints a grant and has nothing to spend it on.
        let fault = plan(&grant("worker-a"), None).unwrap_err();
        assert_eq!(
            fault,
            DialFault::NoDoor {
                worker_fp: "worker-a".to_owned()
            }
        );
        assert!(
            fault.to_string().contains("worker-a"),
            "the report must name the worker the credential is stranded on: {fault}"
        );
    }

    #[test]
    fn a_door_for_another_worker_is_refused_before_a_socket_opens() {
        // The browser's machine runs worker-b; the coordinator minted for
        // worker-a. Presenting A's secret on B's socket is the failure, and
        // `admit_ready` catching it afterwards would leave a foreign worker
        // holding a credential this document never spends again.
        assert_eq!(
            plan(
                &grant("worker-a"),
                Some(&door("http://127.0.0.1:4114", "worker-b")),
            ),
            Err(DialFault::ForeignDoor {
                worker_fp: "worker-a".to_owned(),
                door_worker_fp: "worker-b".to_owned(),
            })
        );
    }

    #[test]
    fn an_origin_the_door_contract_will_not_dial_is_refused() {
        assert_eq!(
            plan(
                &grant("worker-a"),
                Some(&door("file:///etc/passwd", "worker-a")),
            ),
            Err(DialFault::UnusableOrigin {
                origin: "file:///etc/passwd".to_owned()
            })
        );
    }
}
