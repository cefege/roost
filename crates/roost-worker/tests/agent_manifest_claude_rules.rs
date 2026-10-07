//! Claude Code's screen rules, classified against representative real
//! surfaces: the spinner and activity titles, the live turn rows, the
//! permission and selection prompts, the composer. Sibling of
//! `agent_manifest_rules`, which owns the shared harness conventions.

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

#[test]
fn claude_braille_spinner_title_is_working() {
    let rows = ["✳ Refactoring the detector…", ""];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, "⠸ Refactoring the detector"),
        (WORKING, Some("osc_title_working"), false, true, false)
    );
}

#[test]
fn claude_half_circle_spinner_title_is_working() {
    let rows = ["✳ Thinking…", ""];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, "◐ Thinking"),
        (WORKING, Some("osc_title_working"), false, true, false)
    );
}

#[test]
fn claude_live_turn_rows_are_working() {
    let rows = [
        "✳ Reading crates/roost-worker/src/lib.rs",
        "· Reading crates/roost-worker/src/lib.rs… (3s · ↑ 2.1k tokens)",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (WORKING, Some("live_turn_working"), false, true, false)
    );
}

#[test]
fn claude_interrupt_hint_is_working() {
    let rows = ["⏵ esc to interrupt · 42s", ""];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (WORKING, Some("live_turn_working"), false, true, false)
    );
}

#[test]
fn claude_bash_permission_prompt_is_a_visible_blocker() {
    let rows = [
        "Do you want to proceed?",
        "❯ 1. Yes",
        "  2. Yes, and don't ask again for: git push commands",
        "  3. No, and tell Claude what to do differently (esc)",
        "",
    ];
    // The whole-recent region these ports read can no longer tell a live
    // approval from a transcript echo, so the fallback claims it; the state is
    // what a viewer acts on.
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (
            BLOCKED,
            Some("legacy_no_prompt_blocker"),
            false,
            false,
            false
        )
    );
}

#[test]
fn claude_generic_permission_prompt_is_a_visible_blocker() {
    let rows = [
        "Do you want to proceed?",
        "esc to cancel",
        "❯ 1. Yes",
        "  2. No",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (
            BLOCKED,
            Some("generic_permission_prompt"),
            true,
            false,
            false
        )
    );
}

#[test]
fn claude_selection_menu_is_a_visible_blocker() {
    let rows = [
        "Update available!",
        "",
        "❯ 1. Update now",
        "  2. Skip until next version",
        "",
        "enter to select · esc to cancel · ↑/↓ to navigate",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (BLOCKED, Some("live_blocked_form"), true, false, false)
    );
}

#[test]
fn claude_do_you_want_to_prompt_is_a_blocker() {
    let rows = [
        "Apply pending migration to production?",
        "Do you want to proceed?",
        "❯ Yes",
        "  No",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (
            BLOCKED,
            Some("legacy_no_prompt_blocker"),
            false,
            false,
            false
        )
    );
}

#[test]
fn claude_composer_box_is_idle() {
    let rows = ["> apply the pending migration", "", "❯", ""];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (IDLE, Some("live_prompt_box"), false, false, true)
    );
}

#[test]
fn claude_star_spinner_title_is_idle() {
    let rows = [""];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, "✳ Ask Claude to do anything"),
        (IDLE, Some("osc_title_idle"), false, false, true)
    );
}

#[test]
fn claude_permission_prompt_outanks_the_idle_composer_below_it() {
    let rows = [
        "Do you want to proceed?",
        "❯ 1. Yes",
        "  2. No",
        "esc to cancel",
        "",
        "❯",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (
            BLOCKED,
            Some("generic_permission_prompt"),
            true,
            false,
            false
        )
    );
}

#[test]
fn claude_transcript_echo_of_a_permission_prompt_is_not_a_blocker() {
    let rows = [
        "> review the approval prompts",
        "",
        "  earlier transcript: Do you want to proceed? [y/n]",
        "",
        "❯",
        "",
    ];
    assert_eq!(
        outcome(BuiltinAgentId::Claude, &rows, ""),
        (IDLE, Some("live_prompt_box"), false, false, true)
    );
}
