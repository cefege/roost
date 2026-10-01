//! The tests for `voice::state`, split out so the module stays under the cap.

use super::*;

fn listening() -> VoiceMachine {
    let mut machine = VoiceMachine::default();
    let _ = machine.apply(VoiceEvent::Toggle {
        active: true,
        claimed: true,
        engine_available: true,
    });
    let _ = machine.apply(VoiceEvent::Live);
    machine
}

fn start() -> VoiceEvent {
    VoiceEvent::Toggle {
        active: true,
        claimed: true,
        engine_available: true,
    }
}

#[test]
fn a_tap_starts_in_starting_and_only_live_promotes_it() {
    let mut machine = VoiceMachine::default();
    let effects = machine.apply(start());
    assert_eq!(machine.state(), VoiceState::Starting);
    assert_eq!(machine.state().data_state(), "starting");
    assert!(effects.contains(&VoiceEffect::StartEngine));
    assert!(effects.contains(&VoiceEffect::ClaimVoice));
    let _ = machine.apply(VoiceEvent::Live);
    assert_eq!(machine.state(), VoiceState::Listening);
}

#[test]
fn a_tap_while_starting_is_inert() {
    let mut machine = VoiceMachine::default();
    let _ = machine.apply(start());
    assert!(machine.apply(start()).is_empty());
    assert_eq!(machine.state(), VoiceState::Starting);
}

#[test]
fn losing_the_voice_slot_starts_nothing() {
    let mut machine = VoiceMachine::default();
    let effects = machine.apply(VoiceEvent::Toggle {
        active: true,
        claimed: false,
        engine_available: true,
    });
    assert!(effects.is_empty());
    assert_eq!(machine.state(), VoiceState::Idle);
}

#[test]
fn an_inactive_composer_starts_nothing() {
    let mut machine = VoiceMachine::default();
    let _ = machine.apply(VoiceEvent::Toggle {
        active: false,
        claimed: true,
        engine_available: true,
    });
    assert_eq!(machine.state(), VoiceState::Idle);
}

#[test]
fn no_engine_leaves_the_mic_idle_without_starting() {
    let mut machine = VoiceMachine::default();
    let effects = machine.apply(VoiceEvent::Toggle {
        active: true,
        claimed: true,
        engine_available: false,
    });
    assert!(!effects.contains(&VoiceEffect::StartEngine));
    assert_eq!(machine.state(), VoiceState::Idle);
}

#[test]
fn a_second_tap_finalizes_and_arms_the_watchdog() {
    let mut machine = listening();
    let effects = machine.apply(start());
    assert_eq!(machine.state().data_state(), "finalizing");
    assert!(effects.contains(&VoiceEffect::StopAndSend));
    assert!(effects.contains(&VoiceEffect::ArmFinalizeWatchdog));
}

#[test]
fn settling_commits_the_whole_transcript_and_goes_idle() {
    let mut machine = listening();
    machine.apply_transcript("typed base still speaking", "passive dictated line");
    let effects = machine.apply(VoiceEvent::Settled);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert_eq!(
        effects,
        vec![
            VoiceEffect::ClearFinalizeWatchdog,
            VoiceEffect::ClearEngineText,
            VoiceEffect::ReleaseVoice,
            VoiceEffect::Commit("typed base still speaking passive dictated line".to_owned()),
        ]
    );
}

#[test]
fn an_empty_transcript_discards_instead_of_committing() {
    let mut machine = listening();
    let effects = machine.apply(VoiceEvent::Settled);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::DiscardRecording));
    assert!(!effects.iter().any(|e| matches!(e, VoiceEffect::Commit(_))));
}

#[test]
fn a_switch_to_another_pane_commits_settled_words_and_drops_the_hypothesis() {
    let mut machine = listening();
    machine.apply_transcript("hello there", "maybe this");
    let effects = machine.apply(VoiceEvent::Deactivated);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::AbortEngine));
    assert!(effects.contains(&VoiceEffect::Commit("hello there".to_owned())));
    assert!(effects.contains(&VoiceEffect::ReleaseVoice));
}

#[test]
fn the_finalize_watchdog_does_not_let_a_stalled_engine_hang_the_composer() {
    let mut machine = listening();
    machine.apply_transcript("finished speech", "and one more");
    let _ = machine.apply(start());
    let effects = machine.apply(VoiceEvent::WatchdogExpired);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::Commit(
        "finished speech and one more".to_owned()
    )));
    assert!(effects.contains(&VoiceEffect::ClearFinalizeWatchdog));
}

#[test]
fn a_hurried_tap_during_finalizing_finishes_the_recording() {
    let mut machine = listening();
    machine.apply_transcript("final dictated line", "");
    let _ = machine.apply(start());
    assert_eq!(machine.state(), VoiceState::Finalizing);
    let effects = machine.apply(start());
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::AbortEngine));
}

#[test]
fn discarding_aborts_the_engine_and_restores_the_composer() {
    let mut machine = listening();
    machine.apply_transcript("noise", "more noise");
    let effects = machine.apply(VoiceEvent::Discard);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::AbortEngine));
    assert!(effects.contains(&VoiceEffect::DiscardRecording));
    assert!(effects.contains(&VoiceEffect::ReleaseVoice));
}

#[test]
fn an_engine_failure_ends_the_recording_without_committing() {
    let mut machine = listening();
    machine.apply_transcript("half a sentence", "");
    let effects = machine.apply(VoiceEvent::Failed);
    assert_eq!(machine.state(), VoiceState::Idle);
    assert!(effects.contains(&VoiceEffect::DiscardRecording));
    assert!(!effects.iter().any(|e| matches!(e, VoiceEffect::Commit(_))));
}

#[test]
fn a_failure_while_idle_changes_nothing() {
    let mut machine = VoiceMachine::default();
    let _ = machine.apply(VoiceEvent::Failed);
    let _ = machine.apply(VoiceEvent::Discard);
    let _ = machine.apply(VoiceEvent::Deactivated);
    let _ = machine.apply(VoiceEvent::Live);
}

#[test]
fn the_live_update_splits_settled_words_from_the_hypothesis() {
    let mut machine = listening();
    machine.apply_transcript("hello from the mic", "still recording");
    let update = machine.live_update();
    assert_eq!(update.settled, "hello from the mic");
    assert_eq!(update.hypothesis, "still recording");
    assert!(!update.is_empty());
}

#[test]
fn a_second_recording_does_not_inherit_the_first_ones_words() {
    let mut machine = listening();
    machine.apply_transcript("first recording", "");
    let _ = machine.apply(VoiceEvent::Settled);
    let effects = machine.apply(start());
    assert!(effects.contains(&VoiceEffect::ClearEngineText));
    assert_eq!(machine.live_update(), Default::default());
}

#[test]
fn dictating_is_true_for_every_non_idle_state() {
    assert!(!VoiceState::Idle.is_dictating());
    assert!(VoiceState::Starting.is_dictating());
    assert!(VoiceState::Listening.is_dictating());
    assert!(VoiceState::Finalizing.is_dictating());
}
