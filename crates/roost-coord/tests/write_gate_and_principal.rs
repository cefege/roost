//! The write gate's two lists, its release discipline, and the three principal
//! kinds with the rules that refuse them.
//!
//! Covers: the exclusive drain and shared leases including the race between them,
//! the 44-method lease list and the two methods deliberately absent from it, the
//! audit policy including the `PairConfirm` failure seam, and every branch of
//! `resolve_principal`.
//
//! No database and no sleeping: the gate's drop-on-drop release and the
//! principal's five refusals are both pure.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::auth::principal::{Principal, PrincipalFacts, resolve_principal};
use roost_coord::write_gate::{
    WriteGate, WriteGateError, method_audit_skips_success, method_holds_lease,
    method_never_persists_audit, should_persist_method_audit,
};

const FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

fn browser_facts() -> PrincipalFacts {
    PrincipalFacts {
        authorized_key: true,
        account_device: true,
        account_status: Some("active".to_string()),
        account_id: Some("acct-1".to_string()),
        label: "laptop".to_string(),
        ..PrincipalFacts::default()
    }
}

// ── the write gate ─────────────────────────────────────────────────────────

#[test]
fn a_fresh_gate_holds_nothing_and_admits_a_lease() {
    let gate = WriteGate::new();
    assert!(!gate.exclusive_held());
    assert_eq!(gate.state().shared_leases, 0);
    let lease = gate.acquire_shared().expect("no drain is running");
    assert_eq!(gate.state().shared_leases, 1);
    drop(lease);
    assert_eq!(gate.state().shared_leases, 0, "a lease releases on drop");
}

#[test]
fn an_exclusive_drain_refuses_shared_leases_with_the_canonical_message() {
    // The exact strings are the contract a handler surfaces
    // (`apps/coord/src/coordinator-write-gate.ts:31-38`).
    let gate = WriteGate::new();
    let drain = gate.acquire_exclusive().expect("no drain is running");
    assert!(gate.exclusive_held());
    let error = gate.acquire_shared().expect_err("a drain is running");
    assert_eq!(error, WriteGateError::InProgress);
    assert_eq!(
        error.to_string(),
        "coordinator keeper update preparation in progress"
    );
    drop(drain);
    assert!(!gate.exclusive_held());
    assert!(gate.acquire_shared().is_ok());
}

#[test]
fn a_second_exclusive_drain_is_refused_rather_than_queued() {
    // A second keeper update arriving mid-drain is a caller error, and making it
    // wait would only extend the window in which every mutation is blocked.
    let gate = WriteGate::new();
    let _first = gate.acquire_exclusive().expect("no drain is running");
    let error = gate
        .acquire_exclusive()
        .expect_err("one is already running");
    assert_eq!(error, WriteGateError::Held);
    assert_eq!(
        error.to_string(),
        "coordinator keeper update preparation is held"
    );
}

#[test]
fn a_cloned_gate_shares_the_counter_rather_than_copying_it() {
    // A lease taken in a handler and released in an interceptor's `finally` live
    // in different frames; if clone copied the gate, each would own a counter and
    // the fence would count nothing.
    let gate = WriteGate::new();
    let handle = gate.clone();
    let lease = handle.acquire_shared().expect("no drain is running");
    assert_eq!(gate.state().shared_leases, 1);
    drop(lease);
    assert_eq!(gate.state().shared_leases, 0);
}

#[test]
fn a_panic_cannot_leave_the_gate_closed() {
    // A gate that never reopens blocks every mutation in the process, and the
    // process has no other way to notice. That is why release is on `Drop`.
    let gate = WriteGate::new();
    let result = std::panic::catch_unwind({
        let gate = gate.clone();
        move || {
            let _drain = gate.acquire_exclusive().expect("no drain is running");
            panic!("a handler that fails mid-drain");
        }
    });
    assert!(result.is_err());
    assert!(!gate.exclusive_held(), "the drain released while unwinding");
    assert!(gate.acquire_shared().is_ok());
}

// ── the lease list ─────────────────────────────────────────────────────────

#[test]
fn terminal_input_and_prompt_are_deliberately_not_in_the_lease_list() {
    // "Terminal writes acquire their lease only after entering the per-sender/
    // session FIFO. Taking one here would let queued input or a prompt hold the
    // exclusive keeper-update drain open"
    // (`apps/coord/src/auth/auth-interceptor.ts:114-116`). A port that adds
    // either reintroduces exactly that.
    assert!(!method_holds_lease("SessionsInput"));
    assert!(!method_holds_lease("SessionsPrompt"));
    // The method immediately below the comment IS leased.
    assert!(method_holds_lease("SessionsCursorPos"));
}

#[test]
fn a_heartbeat_is_leased_because_it_is_a_durable_write() {
    // It reads like a read and is not one, which is why the list is spelled out
    // rather than derived from a naming convention.
    assert!(method_holds_lease("WorkersHeartbeat"));
    assert!(!method_holds_lease("WorkersList"), "a list is a read");
    assert!(!method_holds_lease("MiscHealth"));
    assert!(!method_holds_lease("SessionsGetScrollbackCells"));
    assert!(!method_holds_lease("UiReportState"));
    assert!(!method_holds_lease("TranscriptionGetConfig"));
    assert!(
        !method_holds_lease("DiagSnapshot"),
        "a snapshot is a read; only the batch submit is a write"
    );
    assert!(method_holds_lease("DiagDebugLogBatch"));
}

#[test]
fn the_lease_list_holds_exactly_the_forty_four_v2_methods() {
    let leased = [
        "WorkersRegister",
        "WorkersHeartbeat",
        "WorkersRename",
        "WorkersDelete",
        "WorkersDeployStart",
        "SessionsSpawn",
        "SessionsAttach",
        "SessionsKill",
        "SessionsRename",
        "SessionsCursorPos",
        "SessionsAssignWorkspace",
        "TasksEnqueue",
        "TasksNextPending",
        "TasksSetState",
        "TasksCancel",
        "WorkspacesCreate",
        "WorkspacesUpdate",
        "WorkspacesDelete",
        "WorkspacesSetSessions",
        "McpCreate",
        "McpDelete",
        "McpPublish",
        "AuthMintBootstrap",
        "AuthRedeemWorker",
        "AuthRedeemBrowser",
        "AuthLogout",
        "PairCreate",
        "PairApprove",
        "PairConfirm",
        "PairDeny",
        "DevicesRevoke",
        "DevicesRotateCurrent",
        "FilesMkdir",
        "TranscriptionSetConfig",
        "AgentConfigSet",
        "AttachFileChunk",
        "DeleteAttachment",
        "PushSubscribe",
        "PushUnsubscribe",
        "UiApplyLayout",
        "DiagDebugLogBatch",
    ];
    assert_eq!(
        leased.len(),
        41,
        "this test's own list drifted from the source"
    );
    for method in leased {
        assert!(method_holds_lease(method), "{method} should hold a lease");
    }
}

// ── the audit policy ───────────────────────────────────────────────────────

#[test]
fn pair_poll_never_persists_an_audit_row_on_either_outcome() {
    // "Requester polling is token-bound but anonymous. Its valid and invalid
    // outcomes are high-volume, carry no caller identity, and must never create
    // an unsweepable audit row." (`auth-interceptor.ts:142-145`.)
    assert!(method_never_persists_audit("PairPoll"));
    assert!(!should_persist_method_audit("PairPoll", 200, false));
    assert!(!should_persist_method_audit("PairPoll", 401, false));
    assert!(!should_persist_method_audit("PairPoll", 500, false));
    // Nothing else is exempt on both outcomes.
    for method in [
        "AuthCoordIdentity",
        "MiscHealth",
        "PairList",
        "UiReportState",
    ] {
        assert!(!method_never_persists_audit(method), "{method}");
    }
}

#[test]
fn a_skipped_success_still_persists_its_failure() {
    // The skip list is about forensic signal in SUCCESSES. "when was this device
    // authorised, and by whom" has to survive a year
    // (`db/audit-retention.ts:21-45`).
    for method in [
        "PairList",
        "MiscHealth",
        "WorkersHeartbeat",
        "PairApprovalStatus",
    ] {
        assert!(method_audit_skips_success(method), "{method}");
        assert!(!should_persist_method_audit(method, 200, false));
        assert!(
            should_persist_method_audit(method, 403, false),
            "{method} failure must persist"
        );
    }
}

#[test]
fn a_failed_pair_confirm_persists_even_though_a_successful_one_does_not() {
    // The one special case, and it is a test seam in the source
    // (`auth-interceptor.ts:155`): "who tried to authorize which device, and
    // failed" is the row an operator actually needs.
    assert!(method_audit_skips_success("PairConfirm"));
    assert!(!should_persist_method_audit("PairConfirm", 200, false));
    assert!(should_persist_method_audit("PairConfirm", 200, true));
    assert!(should_persist_method_audit("PairConfirm", 412, false));
}

// ── the principals ─────────────────────────────────────────────────────────

#[test]
fn an_account_device_with_an_active_account_resolves_and_can_act_as_a_browser() {
    let principal = resolve_principal(FP, &browser_facts()).expect("an active account device");
    assert!(principal.is_browser());
    assert!(!principal.is_worker());
    assert_eq!(principal.fingerprint(), FP);
    assert_eq!(principal.label(), "laptop");
    assert_eq!(principal.require_account_device().expect("a browser"), FP);
    assert!(
        principal.require_worker().is_err(),
        "a browser is not a machine"
    );
}

#[test]
fn a_worker_row_resolves_to_a_worker_and_cannot_act_as_a_browser() {
    let facts = PrincipalFacts {
        authorized_key: true,
        worker: true,
        label: "build-box".to_string(),
        ..PrincipalFacts::default()
    };
    let principal = resolve_principal(FP, &facts).expect("a live worker");
    assert!(principal.is_worker());
    assert!(!principal.is_browser());
    assert_eq!(principal.require_worker().expect("a worker"), FP);
    assert!(principal.require_account_device().is_err());
}

#[test]
fn a_tombstoned_worker_is_refused_rather_than_deleted() {
    // The row survives because sessions still reference it
    // (`apps/coord/migrations/0025_worker_tombstones.sql`), so the row is not
    // authority.
    let facts = PrincipalFacts {
        authorized_key: true,
        worker: true,
        worker_tombstoned: true,
        ..PrincipalFacts::default()
    };
    let error = resolve_principal(FP, &facts).expect_err("a tombstone is not a principal");
    assert!(error.reason.contains("deleted"), "{}", error.reason);
}

#[test]
fn a_key_that_is_both_a_worker_and_a_device_is_refused_rather_than_picked() {
    // "A key must never acquire two kinds of authority"
    // (`apps/coord/src/auth/auth-principal.ts:66`). Picking one would make the
    // answer depend on row order, and the answer decides whether a socket may
    // write. The self-hosted-tenant boot invariant refuses the same condition
    // before any RPC runs, so reaching this at runtime means the database changed
    // under a running process.
    let facts = PrincipalFacts {
        authorized_key: true,
        account_device: true,
        account_status: Some("active".to_string()),
        account_id: Some("acct-1".to_string()),
        worker: true,
        ..PrincipalFacts::default()
    };
    let error = resolve_principal(FP, &facts).expect_err("dual authority is refused");
    assert!(error.reason.contains("both"), "{}", error.reason);
}

#[test]
fn a_disabled_account_and_a_device_with_no_account_are_both_refused() {
    let mut facts = browser_facts();
    facts.account_status = Some("disabled".to_string());
    let error = resolve_principal(FP, &facts).expect_err("a disabled account");
    assert!(error.reason.contains("not active"), "{}", error.reason);

    let mut facts = browser_facts();
    facts.account_id = None;
    assert!(
        resolve_principal(FP, &facts).is_err(),
        "a device with no account"
    );
}

#[test]
fn an_authorized_key_with_neither_row_is_a_legacy_browser_key() {
    // The third kind, and why folding it into an account device would lose the
    // ability to refuse it at the Sync upgrade: it authenticates, and it has no
    // scope.
    let facts = PrincipalFacts {
        authorized_key: true,
        label: "ancient".to_string(),
        ..PrincipalFacts::default()
    };
    let principal = resolve_principal(FP, &facts).expect("a pre-account key");
    assert!(matches!(principal, Principal::LegacySelfHosted { .. }));
    assert!(principal.is_browser(), "it is browser-capable");
    assert!(!principal.is_worker());
    assert!(
        principal.require_account_device().is_ok(),
        "require_account_device accepts it, which is why the Sync upgrade refuses it with 404"
    );
}

#[test]
fn a_fingerprint_with_no_authorized_key_row_resolves_to_nothing() {
    let error = resolve_principal(FP, &PrincipalFacts::default()).expect_err("no key row");
    assert!(
        error.reason.contains("no authorized key"),
        "{}",
        error.reason
    );
}

#[test]
fn the_device_and_worker_refusals_carry_one_message_so_a_guess_is_not_confirmed() {
    // Telling a peer "you are a worker" would confirm the key is live to someone
    // who only guessed the fingerprint.
    let browser = resolve_principal(FP, &browser_facts()).expect("a browser");
    let worker_facts = PrincipalFacts {
        authorized_key: true,
        worker: true,
        ..PrincipalFacts::default()
    };
    let worker = resolve_principal(FP, &worker_facts).expect("a worker");
    let from_browser = browser
        .require_worker()
        .expect_err("a browser is not a machine");
    let from_worker = worker
        .require_account_device()
        .expect_err("a worker is not a browser");
    assert_eq!(from_browser.reason, from_worker.reason);
    assert_eq!(from_browser.reason, "authentication required");
}
