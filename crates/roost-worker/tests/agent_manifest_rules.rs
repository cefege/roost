//! OMP screen/title detection: lifecycle priority, bottom-of-screen evidence,
//! and plain idle when the proved process has no current status evidence.
//! Exercises the pinned rules through the same evaluator as the detector.

use roost_protocol::wire::agent_status::AgentRuntimeState as State;
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::manifest_engine::{DetectionInput, ManifestDetection, evaluate_manifest};
use roost_worker::agents::manifests::AgentManifests;

fn detect(screen: &str, osc_title: &str) -> ManifestDetection {
    let manifests = AgentManifests::pinned().expect("the pinned manifests compile");
    evaluate_manifest(
        manifests.get(BuiltinAgentId::Omp),
        &DetectionInput {
            screen,
            osc_title,
            osc_progress: "",
        },
    )
}

#[test]
fn attention_title_outranks_working_screen_evidence() {
    let detection = detect("Working...\nesc to interrupt", "π ! review the patch");
    assert_eq!(detection.state, Some(State::Blocked));
    assert!(detection.visible_blocker);
    assert!(!detection.visible_working);
}

#[test]
fn working_title_remains_visible_without_screen_output() {
    let detection = detect("", "π ⠋ building");
    assert_eq!(detection.state, Some(State::Working));
    assert!(detection.visible_working);
}

#[test]
fn screen_working_evidence_outranks_a_retained_idle_title() {
    for screen in ["Working...", "esc to interrupt"] {
        let detection = detect(screen, "π > repo");
        assert_eq!(detection.state, Some(State::Working));
        assert!(detection.visible_working);
    }
}

#[test]
fn old_working_output_above_the_live_footer_is_not_current_activity() {
    let detection = detect(
        "Working...\n\nfinished\nresult\nπ > prompt\nfooter\n\n",
        "π > repo",
    );
    assert_eq!(detection.state, Some(State::Idle));
    assert!(detection.visible_idle);
}

#[test]
fn blank_padding_does_not_hide_current_working_evidence() {
    let detection = detect("result\nWorking...\n\n\n", "");
    assert_eq!(detection.state, Some(State::Working));
    assert!(detection.visible_working);
}

#[test]
fn a_proved_process_without_current_evidence_is_plain_idle() {
    let detection = detect("old output: approval required", "unrelated shell title");
    assert_eq!(detection.state, Some(State::Idle));
    assert_eq!(detection.matched_rule_id, None);
    assert!(!detection.visible_idle);
}
