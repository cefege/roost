//! Whether a build with no `roost-keeper` beside its `roost` may install anyway.
//!
//! Split out of the constructor because `artifact_version` is a compile-time
//! constant, so a test binary is always `dev` and the release refusal is
//! unreachable through `LocalPrograms::of_this_process`. What is under test
//! here is the DECISION, in all three of its cases, and the refusal names the
//! directory it looked in — because a refusal that does not say where it
//! looked sends the operator to the wrong place.

use std::path::Path;

use roost_cli::quickstart::install::require_keeper_for_release;

#[test]
fn a_release_build_with_no_keeper_beside_its_roost_is_refused() {
    let refusal = require_keeper_for_release(None, Path::new("/opt/roost/bin/roost"), "3.0.0")
        .expect_err("a release with no keeper must not install");
    let message = refusal.to_string();
    assert!(
        message.contains("roost-keeper"),
        "the refusal must name the program it is missing: {message}"
    );
    assert!(
        message.contains("/opt/roost/bin"),
        "the refusal must name the directory it looked in, or the operator \
         has nowhere to look: {message}"
    );
    assert!(
        message.contains("3.0.0"),
        "the refusal must name the build it refused, so a source build that \
         reads it knows it is not the case it is in: {message}"
    );
}

#[test]
fn a_source_build_with_no_keeper_still_installs() {
    // The case the field was optional for: a development tree ships no separate
    // keeper binary, and refusing here would make `roost quickstart` unusable
    // from a checkout.
    require_keeper_for_release(None, Path::new("/src/target/debug/roost"), "dev")
        .expect("a source build with no keeper is the documented case");
}

#[test]
fn a_release_build_with_a_keeper_beside_its_roost_installs() {
    require_keeper_for_release(
        Some(Path::new("/opt/roost/bin/roost-keeper")),
        Path::new("/opt/roost/bin/roost"),
        "3.0.0",
    )
    .expect("a present keeper is never a reason to refuse");
}

#[test]
fn the_source_build_exemption_is_the_stamp_and_nothing_wider() {
    // A version that merely LOOKS like a dev build must not be exempt: the
    // exemption is the exact stamp, not a prefix, or a `3.0.0-dev.1` candidate
    // would install a worker with no keeper and say nothing.
    for version in ["3.0.0-dev.1", "dev.1", "0.0.0", ""] {
        assert!(
            require_keeper_for_release(None, Path::new("/opt/roost/bin/roost"), version).is_err(),
            "{version:?} is not the dev stamp and must not be exempt"
        );
    }
    require_keeper_for_release(None, Path::new("/opt/roost/bin/roost"), "dev")
        .expect("the exact stamp is exempt");
}
