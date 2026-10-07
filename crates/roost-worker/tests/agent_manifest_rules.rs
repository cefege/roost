//! Screen-rule contract for the pinned agent manifests: a realistic visible
//! grid in, one state + rule id out. Guards the false-state classes the rules
//! exist to kill — a codex composer under an old approval echo, the codex
//! trust/update gates, cursor's `run … (y)` affordance, copilot's
//! background-agent wait. Mirrors v2 `agent-status-manifest-rules.test.ts` and
//! the "pinned manifest engine" cases of `agent-status.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::agent_status::AgentRuntimeState;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::manifest_engine::{DetectionInput, ManifestDetection, evaluate_manifest};
use roost_worker::agents::manifests::AgentManifests;

const WORKING: Option<AgentRuntimeState> = Some(AgentRuntimeState::Working);
const BLOCKED: Option<AgentRuntimeState> = Some(AgentRuntimeState::Blocked);
const IDLE: Option<AgentRuntimeState> = Some(AgentRuntimeState::Idle);

fn detect(
    agent: BuiltinAgentId,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> ManifestDetection {
    let manifests = AgentManifests::pinned().expect("the pinned manifests compile");
    evaluate_manifest(
        manifests.get(agent),
        &DetectionInput {
            screen,
            osc_title,
            osc_progress,
        },
    )
}

/// `(state, rule, visible_blocker, visible_working, visible_idle)` for grid
/// rows as the detector reads them: trailing padding trimmed, blank rows kept,
/// joined by `\n` with no trailing newline.
fn outcome(
    agent: BuiltinAgentId,
    rows: &[&str],
    osc_title: &str,
) -> (
    Option<AgentRuntimeState>,
    Option<&'static str>,
    bool,
    bool,
    bool,
) {
    let detection = detect(agent, &rows.join("\n"), osc_title, "");
    (
        detection.state,
        detection.matched_rule_id,
        detection.visible_blocker,
        detection.visible_working,
        detection.visible_idle,
    )
}

const IDLE_BY_TITLE: (Option<AgentRuntimeState>, Option<&str>, bool, bool, bool) =
    (IDLE, Some("osc_title_idle"), false, false, true);

const CODEX_UPDATE_CHOOSER: [&str; 7] = [
    "Update available! 0.153.0 -> 9.8.7",
    "Run bun add -g @openai/codex to update.",
    "",
    "› 1. Update now",
    "  2. Skip until next version",
    "",
    "Press enter to continue",
];

#[test]
fn codex_idle_at_its_composer_ignores_an_approval_echoed_earlier_in_the_transcript() {
    let rows = [
        "› apply the pending migration",
        "• Ran bash -lc 'ls migrations'",
        "  apply.sql  rollback.sql",
        "• Edited migrations/apply.sql",
        "  Overwrite migrations/apply.sql? [y/n] y",
        "✓ Applied migrations/apply.sql",
        "",
        "› Ask Codex to do anything",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex — roost"),
        IDLE_BY_TITLE
    );
}

#[test]
fn codex_weak_approval_heuristics_still_fire_when_the_transcript_is_live() {
    let rows = [
        "› apply the pending migration",
        "• Ran bash -lc 'psql -f migrations/apply.sql'",
        "  Apply pending migration to production? [y/n]",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex — roost"),
        (BLOCKED, Some("weak_blocker"), false, false, false)
    );
}

#[test]
fn codex_first_run_trust_directory_prompt_is_a_visible_blocker() {
    let rows = [
        "> You are in /home/almalinux/repos/roost",
        "",
        "Do you trust the contents of this",
        "directory? Working with untrusted",
        "contents comes with higher risk of",
        "prompt injection. Trusting the",
        "directory allows project-local config,",
        "hooks, and exec policies to load.",
        "",
        "› 1. Yes, continue",
        "  2. No, quit",
        "",
        "Press enter to continue",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex"),
        (BLOCKED, Some("trust_directory"), true, false, false)
    );
}

#[test]
fn codex_trust_directory_text_quoted_inside_the_transcript_is_not_a_blocker() {
    let rows = [
        "› what does the codex trust prompt look like?",
        "• Explored docs/onboarding.md",
        "> You are in /home/almalinux/repos/roost",
        "Do you trust the contents of this directory?",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex — roost"),
        IDLE_BY_TITLE
    );
}

#[test]
fn codex_startup_update_chooser_is_a_visible_blocker() {
    let rows = [&CODEX_UPDATE_CHOOSER[..], &["", ""]].concat();
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex"),
        (BLOCKED, Some("startup_update"), true, false, false)
    );
}

#[test]
fn codex_composer_below_a_dismissed_update_chooser_is_idle() {
    let rows = [
        &CODEX_UPDATE_CHOOSER[..],
        &["", "› Ask Codex to do anything", ""],
    ]
    .concat();
    assert_eq!(
        outcome(BuiltinAgentId::Codex, &rows, "codex — roost"),
        IDLE_BY_TITLE
    );
}

#[test]
fn cursor_plan_line_beginning_with_run_is_not_an_approval_prompt() {
    let rows = [
        "● I'll run the test suite and report the failures.",
        "",
        "  run the test suite",
        "  ⬡ Thinking",
        "",
        "  ctrl+c to stop",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Cursor, &rows, "cursor"),
        (WORKING, Some("stop_hint_working"), false, true, false)
    );
}

#[test]
fn cursor_run_approval_carrying_the_y_affordance_is_a_visible_blocker() {
    let rows = [
        "● Run terminal command",
        "",
        "  → run bun test apps/worker (y)",
        "    run in background (b)",
        "    reject (esc)",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Cursor, &rows, "cursor"),
        (BLOCKED, Some("approval_prompt"), true, false, false)
    );
}

#[test]
fn copilot_waiting_on_background_agents_is_working_with_no_cancel_hint_on_screen() {
    let rows = [
        "● Delegated 2 tasks to background agents",
        "",
        "◎ Waiting for background agents · 2 running",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Copilot, &rows, "copilot"),
        (
            WORKING,
            Some("background_agents_working"),
            false,
            true,
            false
        )
    );
}

#[test]
fn copilot_background_agent_wait_outranks_the_generic_cancel_hint() {
    let rows = [
        "● Delegated 2 tasks to background agents",
        "",
        "◎ Waiting for background agents",
        "",
        "  esc to cancel · ctrl+c to exit",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Copilot, &rows, "copilot"),
        (
            WORKING,
            Some("background_agents_working"),
            false,
            true,
            false
        )
    );
}

#[test]
fn detects_working_fixtures_for_all_eleven_built_ins() {
    let fixtures = [
        (BuiltinAgentId::Codex, "", "codex ⠋ task", ""),
        (BuiltinAgentId::Claude, "⏸ esc to interrupt · 12s", "", ""),
        (BuiltinAgentId::Gemini, "esc to cancel", "", ""),
        (BuiltinAgentId::OpenCode, "press esc to interrupt", "", ""),
        (BuiltinAgentId::Cursor, "ctrl+c to stop", "", ""),
        (BuiltinAgentId::Amp, "", "⠋ task", ""),
        (BuiltinAgentId::Copilot, "esc again to cancel", "", ""),
        (BuiltinAgentId::Droid, "⠋ Running\nesc to stop", "", ""),
        (BuiltinAgentId::Grok, "", "", "4;1;-1"),
        (BuiltinAgentId::Pi, "Working...", "", ""),
        (BuiltinAgentId::Omp, "", "π ⠋ task", ""),
    ];
    assert_eq!(fixtures.len(), BuiltinAgentId::ALL.len());
    for (agent, screen, title, progress) in fixtures {
        assert_eq!(
            detect(agent, screen, title, progress).state,
            WORKING,
            "{agent:?}"
        );
    }
}

#[test]
fn honors_blocker_priority_visible_idle_and_skip_state_screens() {
    let blocked = detect(
        BuiltinAgentId::Codex,
        "• Working (esc to interrupt)",
        "Action Required",
        "",
    );
    assert_eq!(blocked.state, BLOCKED);
    let idle = detect(BuiltinAgentId::Omp, "", "π > repo", "");
    assert_eq!((idle.state, idle.visible_idle), (IDLE, true));
    let skipped = detect(
        BuiltinAgentId::Codex,
        "› prompt\n↑/↓ to scroll pgup/pgdn to move home/end to jump q to quit esc to edit prev",
        "",
        "",
    );
    assert_eq!((skipped.state, skipped.skip_state_update), (None, true));
}

#[test]
fn defaults_a_known_process_to_idle_rather_than_matching_transcript_identity() {
    let detection = detect(
        BuiltinAgentId::Gemini,
        "old output: codex esc to interrupt",
        "",
        "",
    );
    assert_eq!((detection.state, detection.matched_rule_id), (IDLE, None));
}

#[test]
fn every_agent_is_evaluated_against_its_own_manifest() {
    let manifests = AgentManifests::pinned().expect("the pinned manifests compile");
    for agent in BuiltinAgentId::ALL {
        assert_eq!(manifests.get(agent).id(), agent);
    }
}
