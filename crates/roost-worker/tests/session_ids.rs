//! The session layer's minted identities, and the cwd a record reports. Both
//! are read by a spawn before anything is opened, so a wrong value here is a
//! session the coordinator cannot address or a folder that splits in two.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::session::ids::mint_uuid;
use roost_worker::session::spawn::canonical_session_cwd;

/// The cwd a record reports is the physical one. A symlinked request otherwise
/// disagrees with the path the shell itself emits over OSC 7 a moment later,
/// and one directory then splits into two folder groups in the SPA.
#[test]
fn the_recorded_cwd_is_the_physical_path_not_the_request() {
    let requested = std::env::temp_dir();
    let physical = std::fs::canonicalize(&requested).expect("the temp dir exists");
    let resolved = canonical_session_cwd(requested.to_str().expect("utf-8 temp dir"), None);
    assert_eq!(resolved, physical.to_string_lossy());

    // A folder that does not exist keeps the expanded value: the spawn then
    // fails with the real error instead of having it masked here.
    let missing = canonical_session_cwd("/nope/roost/does/not/exist", None);
    assert_eq!(missing, "/nope/roost/does/not/exist");
}

/// A minted session identity is a uuid the coordinator can key a row on, so a
/// malformed one is a refusal rather than a session nobody can address.
#[test]
fn a_minted_session_id_is_a_uuid_the_coordinator_can_address() {
    let id = mint_uuid().expect("the host has an entropy source");
    assert_eq!(id.len(), 36, "a minted id is not uuid-shaped: {id}");
    assert_eq!(
        id.as_bytes()[14],
        b'4',
        "a minted id is not version 4: {id}"
    );
    let parsed = roost_protocol::wire::brand::SessionId::try_from(id.clone());
    assert!(
        parsed.is_ok(),
        "a minted id was refused as a session id: {id}"
    );
}
