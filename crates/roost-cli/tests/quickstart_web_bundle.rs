#![cfg(unix)]
//! `roost quickstart --web-dist`: what a run given a bundle would install, what
//! it would put in each definition, and what a run given none says instead.
//!
//! Split from `quickstart_dry_run` because the bundle is a different question
//! from "does a dry run write anything" — one is what the plan carries, the
//! other is what the plan touched — and because this file's fixture is the same
//! machine, declared once in a shared support module rather than twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod quickstart_dry_run_support;

use quickstart_dry_run_support::{TempMachine, tree_snapshot};
use roost_cli::quickstart::endpoint::fresh_endpoint;
use roost_cli::quickstart::plan;
use roost_host::HostPlatform;

/// The bundle is named by BOTH definitions and lands inside the release, and
/// the dry run writes neither. The half that matters most is the second: a dry
/// run that copied the operator's bundle would mutate the very directory they
/// are being asked about.
#[test]
fn a_dry_run_names_the_bundle_in_both_definitions_and_copies_nothing() {
    let machine = TempMachine::new("bundle");
    let env = machine.environment();
    let source = machine.root.join("somewhere/apps/web/dist");
    std::fs::create_dir_all(&source).expect("the bundle directory is created");
    std::fs::write(source.join("index.html"), b"<html></html>\n").expect("the index is written");

    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let before = tree_snapshot(&machine.root);
    let resolved = plan::resolve_plan(
        &env,
        HostPlatform::Linux,
        endpoint,
        Some(&source),
        None,
        false,
    )
    .expect("the plan resolves with a bundle");

    let web_dir = resolved
        .web_dir
        .clone()
        .expect("a run given a bundle resolves where it would install it");
    assert!(
        web_dir.ends_with("web"),
        "the bundle is the release's own directory, beside bin/: {}",
        web_dir.display()
    );
    let release_dir = resolved
        .coordinator
        .spec
        .program
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the plan's program sits two levels under a release directory");
    assert_eq!(
        web_dir.parent(),
        Some(release_dir),
        "the bundle is the release's own directory, a sibling of the bin/ its executables went \
         into — which is what makes retiring the release retire the page with it"
    );

    for service in [&resolved.coordinator, &resolved.worker] {
        let text = service
            .definition_text(HostPlatform::Linux)
            .expect("a linux unit renders");
        assert!(
            text.contains("ROOST_WEB_DIST_PATH") && text.contains(&web_dir.display().to_string()),
            "both the coordinator and the worker door read this directory, so both definitions \
             name it:\n{text}"
        );
    }

    assert_eq!(
        before,
        tree_snapshot(&machine.root),
        "a dry run copied the bundle, or staged anything beside the release"
    );
    assert!(
        !web_dir.exists(),
        "the destination is a real install's job: {}",
        web_dir.display()
    );
}

/// A run given no bundle leaves the two definitions in DIFFERENT states, and
/// each is the one that role actually reads.
///
/// The coordinator's own resolution writes the setting blank rather than
/// omitting it, and that is correct: an entry absent from a definition falls
/// back to whatever the service manager's own environment holds, which is how a
/// cleared front door comes back from a stale manager value. The worker is
/// stamped only by an install that has a bundle, so it carries nothing at all.
/// A test that asserted the two agreed would have been asserting a tidiness the
/// product does not have and should not acquire.
#[test]
fn a_dry_run_without_a_bundle_leaves_each_definition_saying_nothing_is_served() {
    let machine = TempMachine::new("no-bundle");
    let env = machine.environment();
    let endpoint = fresh_endpoint(None).expect("a loopback endpoint");
    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, None, false)
        .expect("the plan resolves with no bundle");

    assert_eq!(resolved.web_dir, None);
    let coordinator = resolved
        .coordinator
        .definition_text(HostPlatform::Linux)
        .expect("a linux unit renders");
    assert!(
        coordinator.contains("ROOST_WEB_DIST_PATH="),
        "the coordinator's own resolution always writes the setting, so a cleared value is \
         cleared the same way every time:\n{coordinator}"
    );
    assert!(
        !coordinator.contains("ROOST_WEB_DIST_PATH=/"),
        "and it must be blank rather than naming a directory: a path with nothing behind it \
         reports a healthy spa line for a page that is not there:\n{coordinator}"
    );

    let worker = resolved
        .worker
        .definition_text(HostPlatform::Linux)
        .expect("a linux unit renders");
    assert!(
        !worker.contains("ROOST_WEB_DIST_PATH"),
        "the worker is stamped only by an install that has a bundle, so an install without one \
         leaves it unset rather than stamping an empty path:\n{worker}"
    );
}
