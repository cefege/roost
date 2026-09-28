//! Controller mode and the one directional predicate. A pad plugged in for
//! games must not flip the UI: `auto` latches only on real input, and the
//! latch — not "forced on" — is what the Start guide reads. TV or controller
//! either one makes input directional.
//! Pins `apps/web/src/lib/{padMode,directionalInput}.ts`.

use roost_client_core::{KeyValueStore as _, MemoryKeyValueStore};
use roost_web::input_nav::modality::{ModeChoice, NavModality, PAD_MODE_KEY};

#[test]
fn auto_latches_on_the_first_real_input_and_only_once() {
    let mut modality = NavModality::new(ModeChoice::Auto, ModeChoice::Auto, false);
    assert!(
        !modality.pad_mode_active(),
        "connection alone must not flip the mode"
    );
    assert_eq!(modality.root_attributes()[1], ("data-pad", "false"));

    assert!(modality.note_pad_activity());
    assert!(modality.pad_mode_active());
    assert!(modality.pad_input_seen());
    assert_eq!(modality.root_attributes()[1], ("data-pad", "true"));

    assert!(
        !modality.note_pad_activity(),
        "a latched mode does not re-apply"
    );
}

#[test]
fn forcing_the_mode_does_not_pretend_a_pad_was_seen() {
    let storage = MemoryKeyValueStore::new();
    let mut modality = NavModality::load(None, Some("1"), &storage, false);
    assert!(modality.pad_mode_active());
    assert!(!modality.pad_input_seen());
    assert_eq!(storage.get(PAD_MODE_KEY).as_deref(), Some("on"));

    modality.note_pad_activity();
    modality.set_pad_choice(&storage, ModeChoice::Off);
    assert!(!modality.pad_mode_active(), "off wins over the latch");
    assert_eq!(storage.get(PAD_MODE_KEY).as_deref(), Some("off"));
}

#[test]
fn either_modality_makes_input_directional() {
    let desktop = NavModality::new(ModeChoice::Auto, ModeChoice::Auto, false);
    assert!(!desktop.directional_input_active());

    let television = NavModality::new(ModeChoice::On, ModeChoice::Off, false);
    assert!(television.directional_input_active());

    let mut console = NavModality::new(ModeChoice::Off, ModeChoice::Auto, false);
    assert!(!console.directional_input_active());
    console.note_pad_activity();
    assert!(console.directional_input_active());
}
