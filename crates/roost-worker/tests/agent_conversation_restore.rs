//! The worker-owned OMP resume plan (`omp --resume=<ref>`): its POSIX quoting,
//! reference validation, per-pass dedupe with its rejection rollback, the
//! partial-write line discard, and the one-batch keeper acknowledgement truth.
//! No integration text becomes executable syntax, and no reference value
//! reaches a log line. Ports `apps/worker/tests/agents/agent-conversation-restore.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;
mod session_support;

use std::collections::HashSet;

use agent_prompt_support::log_capture::capture_events;
use roost_platform::{HostPlatform, posix_shell_quote};
use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_worker::agents::conversation_restore::{
    AgentConversationRestoreDeps, AgentConversationRestoreOutcome,
    OMP_CONVERSATION_RESUME_DESCRIPTOR_V1, RestoreSkip, conversation_restore_dedupe_key,
    materialize_omp_conversation_restore_input, restore_agent_conversation_after_respawn,
};
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_channels::KeeperInputResult;
use session_support::input_script::ScriptedAnswer;
use session_support::{Harness, SESSION, session_id};

fn reference(kind: AgentConversationReferenceKind, value: &str) -> AgentConversationReferenceV1 {
    AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind,
        value: value.to_owned(),
    }
}

fn live_session() -> Harness {
    let harness = Harness::new();
    harness.install(SESSION, 1, "/home/user/project", "/home/user/project");
    harness
}

fn deps<'a>(
    harness: &'a Harness,
    keys: Option<&'a mut HashSet<String>>,
) -> AgentConversationRestoreDeps<'a> {
    AgentConversationRestoreDeps {
        enabled: true,
        manager: &harness.manager,
        platform: HostPlatform::Linux,
        resumed_reference_keys: keys,
    }
}

fn written(harness: &Harness) -> Vec<Vec<u8>> {
    harness
        .keeper
        .input
        .written()
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect()
}

#[test]
fn the_descriptor_pins_the_official_executable_and_option_form() {
    let descriptor = OMP_CONVERSATION_RESUME_DESCRIPTOR_V1;
    assert_eq!(descriptor.schema_version, 1);
    assert_eq!((descriptor.agent_id, descriptor.executable), ("omp", "omp"));
    assert_eq!(descriptor.fixed_option_prefix, "--resume=");
    assert_eq!(
        descriptor.reference_kinds,
        [
            AgentConversationReferenceKind::Id,
            AgentConversationReferenceKind::Path
        ]
    );
    assert_eq!(
        descriptor.platforms,
        [HostPlatform::MacOs, HostPlatform::Linux]
    );
}

#[cfg(unix)]
#[test]
fn every_reference_stays_one_opaque_argv_element() {
    for agent_reference in [
        reference(AgentConversationReferenceKind::Id, "01J8OMP-prefix"),
        reference(
            AgentConversationReferenceKind::Path,
            "/tmp/omp 'session' $(printf injected); `printf nope`.jsonl",
        ),
    ] {
        let payload =
            materialize_omp_conversation_restore_input(&agent_reference, HostPlatform::Linux)
                .unwrap();
        assert_eq!(payload.last(), Some(&b'\r'));
        let command = String::from_utf8(payload[..payload.len() - 1].to_vec()).unwrap();
        let resume = format!("--resume={}", agent_reference.value);
        assert_eq!(
            command,
            format!(
                "{} {}",
                posix_shell_quote("omp"),
                posix_shell_quote(&resume)
            )
        );
        let shell = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "omp() {{ printf '%s\\0' \"$#\" \"$1\"; }}\n{command}"
            ))
            .output()
            .unwrap();
        assert!(shell.status.success());
        assert!(
            shell.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&shell.stderr)
        );
        assert_eq!(
            String::from_utf8(shell.stdout).unwrap(),
            format!("1\0{resume}\0")
        );
    }
}

#[test]
fn windows_has_no_restore_materialization() {
    let agent_reference = reference(AgentConversationReferenceKind::Id, "opaque");
    assert_eq!(
        materialize_omp_conversation_restore_input(&agent_reference, HostPlatform::Windows),
        None
    );
}

#[test]
fn control_characters_relative_paths_and_oversized_references_are_unsupported() {
    for control in ["\u{0}", "\u{3}", "\u{9}", "\u{a}", "\u{d}", "\u{7f}"] {
        let agent_reference = reference(
            AgentConversationReferenceKind::Id,
            &format!("prefix{control}suffix"),
        );
        assert_eq!(
            materialize_omp_conversation_restore_input(&agent_reference, HostPlatform::Linux),
            None
        );
    }
    for (kind, value) in [
        (
            AgentConversationReferenceKind::Path,
            "relative/session.jsonl".to_owned(),
        ),
        (AgentConversationReferenceKind::Id, "x".repeat(513)),
        (
            AgentConversationReferenceKind::Path,
            format!("/{}", "x".repeat(4096)),
        ),
    ] {
        assert_eq!(
            materialize_omp_conversation_restore_input(
                &reference(kind, &value),
                HostPlatform::Linux
            ),
            None
        );
    }
    let absolute = reference(AgentConversationReferenceKind::Path, "/tmp/session.jsonl");
    assert!(materialize_omp_conversation_restore_input(&absolute, HostPlatform::Linux).is_some());
}

#[tokio::test]
async fn each_keeper_result_is_terminal_after_one_batch_and_never_logs_the_reference() {
    let secret = "/private/opaque-secret.jsonl";
    for (answer, expected) in [
        (None, WorkerInputResult::Accepted { written_bytes: 0 }),
        (
            Some(KeeperInputResult::Reject {
                reason: "queue_full".to_owned(),
            }),
            WorkerInputResult::Rejected {
                reason: "queue_full".to_owned(),
            },
        ),
        (
            Some(KeeperInputResult::Ambiguous {
                written: Some(0),
                reason: "write_error".to_owned(),
            }),
            WorkerInputResult::Ambiguous {
                written_bytes: 0,
                reason: "write_error".to_owned(),
            },
        ),
    ] {
        let (logged, _capture) = capture_events();
        let harness = live_session();
        if let Some(answer) = answer {
            harness
                .keeper
                .input
                .answer_next(ScriptedAnswer::Answered(answer));
        }
        let agent_reference = reference(AgentConversationReferenceKind::Path, secret);
        let payload =
            materialize_omp_conversation_restore_input(&agent_reference, HostPlatform::Linux)
                .unwrap();
        let outcome = restore_agent_conversation_after_respawn(
            deps(&harness, None),
            &session_id(SESSION),
            Some(&agent_reference),
        )
        .await;
        let expected = match expected {
            WorkerInputResult::Accepted { .. } => WorkerInputResult::Accepted {
                written_bytes: payload.len() as u32,
            },
            other => other,
        };
        assert_eq!(outcome, AgentConversationRestoreOutcome::Written(expected));
        assert_eq!(written(&harness), [payload]);
        let transitions: Vec<String> = logged()
            .into_iter()
            .filter(|line| line.starts_with("agent_conversation_restore_transition"))
            .collect();
        assert_eq!(transitions.len(), 1, "{transitions:?}");
        assert!(
            !transitions[0].contains(secret) && !transitions[0].contains("--resume="),
            "{}",
            transitions[0]
        );
    }
}

#[tokio::test]
async fn a_proven_rejection_releases_the_reference_claim_and_an_ambiguous_one_keeps_it() {
    let harness = live_session();
    let input = &harness.keeper.input;
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Reject {
        reason: "queue_full".to_owned(),
    }));
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ambiguous {
        written: Some(0),
        reason: "write_error".to_owned(),
    }));
    let agent_reference = reference(
        AgentConversationReferenceKind::Path,
        "/private/shared.jsonl",
    );
    let key = conversation_restore_dedupe_key(&agent_reference);
    let mut keys = HashSet::new();

    let first = restore_agent_conversation_after_respawn(
        deps(&harness, Some(&mut keys)),
        &session_id(SESSION),
        Some(&agent_reference),
    )
    .await;
    assert!(matches!(
        first,
        AgentConversationRestoreOutcome::Written(WorkerInputResult::Rejected { .. })
    ));
    assert!(!keys.contains(&key));
    let second = restore_agent_conversation_after_respawn(
        deps(&harness, Some(&mut keys)),
        &session_id(SESSION),
        Some(&agent_reference),
    )
    .await;
    assert!(matches!(
        second,
        AgentConversationRestoreOutcome::Written(WorkerInputResult::Ambiguous { .. })
    ));
    assert!(keys.contains(&key));
}

#[tokio::test]
async fn a_partly_delivered_resume_command_is_discarded_from_the_prompt() {
    let (logged, _capture) = capture_events();
    let harness = live_session();
    harness
        .keeper
        .input
        .answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ambiguous {
            written: Some(1),
            reason: "write_error".to_owned(),
        }));
    let secret = "/private/opaque-secret.jsonl";
    let agent_reference = reference(AgentConversationReferenceKind::Path, secret);

    let outcome = restore_agent_conversation_after_respawn(
        deps(&harness, None),
        &session_id(SESSION),
        Some(&agent_reference),
    )
    .await;
    assert_eq!(
        outcome,
        AgentConversationRestoreOutcome::Written(WorkerInputResult::Ambiguous {
            written_bytes: 1,
            reason: "write_error".to_owned()
        })
    );
    let payload =
        materialize_omp_conversation_restore_input(&agent_reference, HostPlatform::Linux).unwrap();
    assert_eq!(written(&harness), [payload, vec![0x03]]);
    let lines = logged();
    let count = |name: &str| {
        lines
            .iter()
            .filter(|line| line.starts_with(&format!("{name} ")))
            .count()
    };
    assert_eq!(count("agent_conversation_restore_transition"), 1);
    assert_eq!(count("agent_conversation_restore_discard_transition"), 1);
    assert!(!lines.join("\n").contains(secret));
}

#[tokio::test]
async fn disabled_missing_and_unsupported_restores_write_zero_input() {
    let harness = live_session();
    let sid = session_id(SESSION);
    let agent_reference = reference(AgentConversationReferenceKind::Id, "opaque");
    let mut disabled = deps(&harness, None);
    disabled.enabled = false;
    let skipped = |skip| AgentConversationRestoreOutcome::Skipped(skip);
    assert_eq!(
        restore_agent_conversation_after_respawn(disabled, &sid, Some(&agent_reference)).await,
        skipped(RestoreSkip::Disabled)
    );
    assert_eq!(
        restore_agent_conversation_after_respawn(deps(&harness, None), &sid, None).await,
        skipped(RestoreSkip::MissingReference)
    );
    let mut windows = deps(&harness, None);
    windows.platform = HostPlatform::Windows;
    assert_eq!(
        restore_agent_conversation_after_respawn(windows, &sid, Some(&agent_reference)).await,
        skipped(RestoreSkip::Unsupported)
    );
    let control = reference(AgentConversationReferenceKind::Id, "opaque\u{15}suffix");
    assert_eq!(
        restore_agent_conversation_after_respawn(deps(&harness, None), &sid, Some(&control)).await,
        skipped(RestoreSkip::Unsupported)
    );
    assert!(written(&harness).is_empty());
}

#[tokio::test]
async fn a_reference_already_claimed_in_this_pass_never_resumes_twice() {
    let harness = live_session();
    let agent_reference = reference(
        AgentConversationReferenceKind::Path,
        "/private/shared.jsonl",
    );
    let mut keys = HashSet::from([conversation_restore_dedupe_key(&agent_reference)]);
    assert_eq!(
        restore_agent_conversation_after_respawn(
            deps(&harness, Some(&mut keys)),
            &session_id(SESSION),
            Some(&agent_reference)
        )
        .await,
        AgentConversationRestoreOutcome::Skipped(RestoreSkip::Duplicate)
    );
    assert!(written(&harness).is_empty());
}
