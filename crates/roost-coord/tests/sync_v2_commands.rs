//! The Sync v2 client command path: the socket-identity fence, the domain
//! transitions a legal socket may make, and the definite answer every refused
//! command owes the client.
//!
//! The rules here are the ones `docs/FAILURE-INDEX.md` records under "A terminal
//! domain reset is treated as the input fence": a refused `input` carries a
//! reason and the command's own domain generation, so the client classifies it as
//! REJECTED and restores its draft instead of claiming possible input loss.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_coord::sync_ws::commands::{
    handle_client_frame, ClientContext, CommandOutcome, TerminalCommand,
};
use roost_coord::sync_ws::admission::EnqueueOutcome;
use roost_coord::sync_ws::domain_table::DomainGenerations;
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{
    InputCommand, SyncClientFrame, SyncDomain, SyncDomainReadyCommand, TerminalInputRouteClaim,
};

const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
const SNAPSHOT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn generations() -> Arc<DomainGenerations> {
    Arc::new(DomainGenerations::new(1_000))
}

fn context() -> ClientContext {
    let mut session_ids = BTreeSet::new();
    session_ids.insert(SESSION_A.to_owned());
    ClientContext {
        read_only: false,
        tab_id: Some("tab-1".to_owned()),
        viewer_key: Some("fingerprint:tab-1".to_owned()),
        fingerprint: "fingerprint".to_owned(),
        session_ids,
    }
}

/// Close the terminal domain's snapshot fence with a real one-time token.
fn hydrate_terminal(
    session: &mut SyncV2Session,
    tokens: &mut SnapshotTokenRegistry,
    token: &str,
) {
    let socket_id = session.socket_id.clone();
    let mut covered = BTreeSet::new();
    covered.insert(SESSION_A.to_owned());
    assert!(tokens.bind(&socket_id, "fingerprint", token, covered));
    let frame = SyncClientFrame {
        ack_delivery_seq: None,
        socket_id,
        command: Some(ClientCommand::DomainReady(Box::new(
            SyncDomainReadyCommand {
                domain: SyncDomain::Terminal.into(),
                generation: session
                    .domain_generation(SyncDomain::Terminal)
                    .expect("a domain exists"),
                snapshot_token: Some(token.to_owned()),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        __buffa_unknown_fields: Default::default(),
    };
    let outcome = handle_client_frame(session, &context(), &frame, tokens, 1_000);
    assert!(matches!(
        outcome,
        CommandOutcome::DomainReady {
            domain: SyncDomain::Terminal,
            ..
        }
    ));
}

fn ready_frame(socket_id: &str, generation: u64, token: Option<&str>) -> SyncClientFrame {
    SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: socket_id.to_owned(),
        command: Some(ClientCommand::DomainReady(Box::new(
            SyncDomainReadyCommand {
                domain: SyncDomain::Terminal.into(),
                generation,
                snapshot_token: token.map(str::to_owned),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        __buffa_unknown_fields: Default::default(),
    }
}

#[allow(dead_code)]
fn client_frame(socket_id: &str, command: Option<ClientCommand>) -> SyncClientFrame {
    SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: socket_id.to_owned(),
        command,
        __buffa_unknown_fields: Default::default(),
    }
}

#[test]
fn a_domain_becomes_ready_once_and_its_token_cannot_be_replayed() {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket("socket-1", "fingerprint");
    let generation = session
        .domain_generation(SyncDomain::Terminal)
        .expect("a domain exists");
    assert!(!session.terminal_domain_ready());

    hydrate_terminal(&mut session, &mut tokens, SNAPSHOT_A);
    assert!(session.terminal_domain_ready());

    // A replayed ready for a domain that is ALREADY ready is a no-op: the
    // domain does not re-hydrate, and the client cannot use the replay to
    // re-admit a session list the coordinator has already moved past.
    let replay = handle_client_frame(
        &mut session,
        &context(),
        &ready_frame("socket-1", generation, Some(SNAPSHOT_A)),
        &mut tokens,
        1_000,
    );
    assert!(
        matches!(replay, CommandOutcome::Nothing),
        "a ready for a ready domain must not re-admit anything"
    );

    // After a reset the token is gone, so the next ready RESETS rather than
    // opening the fence on a snapshot the client no longer matches.
    assert!(matches!(
        session.reset_domain(SyncDomain::Terminal, "test_reset"),
        EnqueueOutcome::Reset(_)
    ));
    let stale_generation = session
        .domain_generation(SyncDomain::Terminal)
        .expect("a domain exists");
    let after_reset = handle_client_frame(
        &mut session,
        &context(),
        &ready_frame("socket-1", stale_generation, Some(SNAPSHOT_A)),
        &mut tokens,
        1_000,
    );
    assert!(
        matches!(after_reset, CommandOutcome::ResetTerminal(_)),
        "a consumed snapshot token must reset the terminal domain, not re-admit it"
    );
    assert!(!session.terminal_domain_ready());
}

#[test]
fn an_illegal_transition_is_refused_and_a_legal_one_is_not() {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket("socket-1", "fingerprint");

    // ILLEGAL: a stale socket id never reaches a command.
    let stale = handle_client_frame(
        &mut session,
        &context(),
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-0".to_owned(),
            command: Some(ClientCommand::Input(Box::new(InputCommand {
                session_id: SESSION_A.to_owned(),
                input_seq: 7,
                data: vec![65],
                domain_generation: 1,
                ..InputCommand::default()
            }))),
            __buffa_unknown_fields: Default::default(),
        },
        &mut tokens,
        1_000,
    );
    assert!(matches!(stale, CommandOutcome::Nothing));

    // ILLEGAL: an input command for a terminal domain that has not closed its
    // snapshot/live gap is REFUSED with a definite answer, not dropped.
    let refused = handle_client_frame(
        &mut session,
        &context(),
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-1".to_owned(),
            command: Some(ClientCommand::Input(Box::new(InputCommand {
                session_id: SESSION_A.to_owned(),
                input_seq: 7,
                data: vec![65],
                domain_generation: 1,
                ..InputCommand::default()
            }))),
            __buffa_unknown_fields: Default::default(),
        },
        &mut tokens,
        1_000,
    );
    let CommandOutcome::Refusal(frame) = refused else {
        panic!("a refused input must carry an answer, not a silence");
    };
    let Some(Frame::InputRejected(rejection)) = frame.frame else {
        panic!("the answer to a refused input is inputRejected");
    };
    assert_eq!(rejection.session_id, SESSION_A);
    assert_eq!(rejection.input_seq, 7);
    assert!(rejection.reason.contains("resubscribing"));

    // LEGAL: once the fence is closed, the same command is admitted.
    hydrate_terminal(&mut session, &mut tokens, SNAPSHOT_A);
    let generation = session
        .domain_generation(SyncDomain::Terminal)
        .expect("a domain exists");
    let admitted = handle_client_frame(
        &mut session,
        &context(),
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-1".to_owned(),
            command: Some(ClientCommand::Input(Box::new(InputCommand {
                session_id: SESSION_A.to_owned(),
                input_seq: 7,
                data: vec![65],
                domain_generation: generation,
                ..InputCommand::default()
            }))),
            __buffa_unknown_fields: Default::default(),
        },
        &mut tokens,
        1_000,
    );
    let CommandOutcome::Terminal(TerminalCommand::Input(input)) = admitted else {
        panic!("an admissible input command must reach the command sink");
    };
    assert_eq!(input.input_seq, 7);
}

#[test]
fn a_frame_with_neither_a_command_nor_an_acknowledgement_is_refused() {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    let outcome = handle_client_frame(
        &mut session,
        &context(),
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-1".to_owned(),
            command: None,
            __buffa_unknown_fields: Default::default(),
        },
        &mut tokens,
        1_000,
    );
    assert!(
        matches!(outcome, CommandOutcome::Invalid),
        "a frame the coordinator cannot interpret must close the socket, not be ignored"
    );
}

#[test]
fn a_route_claim_refusal_is_definite_even_for_a_read_only_socket() {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    let mut read_only = context();
    read_only.read_only = true;
    let outcome = handle_client_frame(
        &mut session,
        &read_only,
        &SyncClientFrame {
            ack_delivery_seq: None,
            socket_id: "socket-1".to_owned(),
            command: Some(ClientCommand::InputRouteClaim(Box::new(
                TerminalInputRouteClaim {
                    request_id: "route-1".to_owned(),
                    session_id: SESSION_A.to_owned(),
                    revision: 1,
                    domain_generation: 1,
                    worker_epoch: "worker-epoch".to_owned(),
                    __buffa_unknown_fields: Default::default(),
                },
            ))),
            __buffa_unknown_fields: Default::default(),
        },
        &mut tokens,
        1_000,
    );
    let CommandOutcome::Refusal(frame) = outcome else {
        panic!("a refused route claim must carry a result");
    };
    let Some(Frame::InputRouteResult(result)) = frame.frame else {
        panic!("the answer to a refused route claim is inputRouteResult");
    };
    assert!(!result.accepted);
    assert_eq!(result.request_id, "route-1");
    assert!(result.reason.contains("cannot write terminal input"));
}

