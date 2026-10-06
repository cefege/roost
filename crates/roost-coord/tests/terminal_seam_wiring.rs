//! `CoordTerminal`'s `Debug` must name the collaborators that are actually
//! wired, and this is the test that stops it regressing.
//!
//! IT IS A DEBUG-OUTPUT TEST AND NOT A BEHAVIOUR TEST because the two
//! collaborators are behaviourally identical on a coordinator with no sessions:
//! a page renders the same either way, and every behavioural assertion passes
//! with the no-op still wired. **`NoTerminalSeams` and `TerminalViewHub` can
//! only be told apart by which one was passed**, so the assertion has to look at
//! the wiring, and this is the shape that can.
//!
//! The no-op's own doc is the warning that makes it necessary rather than
//! pedantic: "two no-op types invite a caller to wire one and forget the
//! other." After the substitution the no-op is CORRECT for a test and WRONG for
//! production, and the only thing that keeps it that way is a test that looks.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::sync::Arc;

use roost_coord::coord_core::seams::CoordTerminal;
use roost_coord::services::CoordServices;
use roost_coord::terminal_screen::byte_hub::ByteHub;
use roost_coord::terminal_view::TerminalViewHub;

fn rendered(terminal: &CoordTerminal) -> String {
    format!("{terminal:?}")
}

#[test]
fn a_seam_with_the_no_op_says_so_and_does_not_name_itself_instead() {
    let line = rendered(&CoordTerminal::none());
    // The defect this file exists for: `type_name::<Self>()` printed
    // "CoordTerminal" for BOTH fields, so a reader saw two plausible type
    // names, concluded the seam was reporting, and stopped looking.
    assert!(
        line.contains("NoTerminalSeams"),
        "the no-op must be named when the no-op is wired: {line}"
    );
    assert!(
        !line.contains("\"CoordTerminal\""),
        "a field still prints the container's own type name: {line}"
    );
}

#[test]
fn a_seam_with_the_real_hubs_names_them_and_not_the_no_op() {
    let line = rendered(&CoordTerminal::new(
        Arc::new(ByteHub::with_defaults()),
        Arc::new(TerminalViewHub::new()),
    ));
    assert!(
        line.contains("ByteHub") && line.contains("TerminalViewHub"),
        "the real collaborators must be named: {line}"
    );
    assert!(
        !line.contains("NoTerminalSeams"),
        "the no-op is not wired here, so naming it would be the same lie: {line}"
    );
}

/// The production wiring, asserted by POINTER IDENTITY.
///
/// `Arc::as_ptr` returns the DATA pointer, not the vtable pointer, so this
/// works straight through the `dyn` with no downcast and no `Any`. It is an
/// unusual assertion and it is the right one here: the two collaborators are
/// behaviourally identical by construction, so the ONLY thing that
/// distinguishes them is which pointer was handed over — and "which pointer was
/// handed over" is the question this whole slice is about.
///
/// It runs against a real `CoordServices` because a hand-built `CoordTerminal`
/// would only prove the test can build one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_seam_wiring_is_the_real_hubs() {
    let root = std::env::temp_dir().join(format!("roost-terminal-seam-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    let database = db_support::open_test_database(&root)
        .await
        .expect("a migrated coordinator database");
    let services = CoordServices::new(database);
    let seam = roost_coord::serve::terminal_seams(&services);

    assert_eq!(
        std::sync::Arc::as_ptr(&seam.routes) as *const (),
        std::sync::Arc::as_ptr(&services.byte_hub) as *const (),
        "the route collaborator must BE the byte hub"
    );
    assert_eq!(
        std::sync::Arc::as_ptr(&seam.views) as *const (),
        std::sync::Arc::as_ptr(&services.views) as *const (),
        "the lifecycle collaborator must BE the view hub, not the no-op"
    );
    assert!(!rendered(&seam).contains("NoTerminalSeams"));
    let _ = std::fs::remove_dir_all(&root);
}
