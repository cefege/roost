//! Switching machines in the folder picker: the directory resets, and a listing
//! that belongs to one machine can never repaint another.
//!
//! A picker is scoped to ONE machine, and the picker on `/browse/:workerFp` is
//! about to open the directory that route names. Three facts have to hold at
//! once, and each is a failure a reader would describe differently:
//!
//! - the directory resets BEFORE anything is asked, so the first listing a new
//!   machine receives already carries that machine's own path rather than the
//!   previous machine's, its resolved path, or its history;
//! - a reply for a machine the reader has left updates that machine and nothing
//!   else, because the fence is keyed by fingerprint;
//! - a reply from a generation the reader has moved past is dropped, so returning
//!   to a machine republishes it rather than reviving what it was showing.
//!
//! These are the assertions behind `smoke/terminal/terminal-delivery.spec.ts`
//! ("new-terminal server switch resets browse path before listing and spawning"),
//! which drives the same three facts through a real coordinator and reads the
//! `FilesListDir` requests the browser actually issued. Here the requests are
//! the requests, with no wire between them and the state.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::store::browse_entries::BrowseEntry;
use roost_client_core::store::browse_machine::BrowseListingRequest;
use roost_client_core::store::browse_state::BrowseState;
use roost_protocol::wire::WorkerFp;

const WORKER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// Machine A's start directory.
const A_HOME: &str = "/home/a";
/// A directory under A's, which the reader drilled into.
const A_DEEP: &str = "/home/a/src";
/// What A's start directory resolved to on that machine, which is a path the
/// other machine has never heard of.
const A_RESOLVED: &str = "/real/a/src";
/// Machine B's start directory.
const B_HOME: &str = "/home/b";
/// A directory under B's, which the reader drilled into.
const B_DEEP: &str = "/home/b/src";

fn worker(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).expect("a worker fingerprint")
}

/// One machine's browser, opened on `home`.
///
/// This is the whole of a server switch: the picker opens the machine the route
/// names, on the directory that route starts at. Nothing about the machine it
/// came from reaches it, because every field of a fresh browser is seeded from
/// `home`.
fn switch_to(browse: &mut BrowseState, machine: &str, home: &str) -> WorkerFp {
    let fingerprint = worker(machine);
    browse.open(&fingerprint, Some(home));
    fingerprint
}

/// Ask one machine for its directory, returning the request and recording it.
///
/// The recording is the request list a browser spec reads off the wire, so a
/// test can assert on what was ASKED and not only on what came back.
fn ask(
    browse: &mut BrowseState,
    machine: &WorkerFp,
    asked: &mut Vec<(String, String)>,
) -> BrowseListingRequest {
    let request = browse
        .begin_listing(machine)
        .expect("an open machine has a listing to ask for");
    let asked_for = (request.worker_fp.as_str().to_owned(), request.path.clone());
    asked.push(asked_for);
    request
}

/// THE RESET HAPPENS BEFORE THE FIRST REQUEST. The machine the picker switches
/// to is opened on ITS OWN directory, and the very first listing it is asked for
/// already names it — including when the machine left drilled into a directory
/// whose resolved path is not a path the new machine has ever heard of.
#[test]
fn a_server_switch_resets_the_path_before_the_first_listing_request() {
    let mut browse = BrowseState::new();
    let a = switch_to(&mut browse, WORKER_A, A_HOME);
    browse.set_cwd(&a, A_DEEP);
    let mut asked = Vec::new();
    let for_a = ask(&mut browse, &a, &mut asked);
    let rows = vec![BrowseEntry::dir("app", 3)];
    let answered = browse.apply_listing(&for_a, A_RESOLVED.to_owned(), rows);
    assert!(answered, "machine A's own listing stands");

    // The switch.
    let b = switch_to(&mut browse, WORKER_B, B_HOME);
    let switched_to = browse.get(&b).expect("machine B is open");
    assert_eq!(switched_to.cwd(), B_HOME, "the reset directory");
    assert_eq!(switched_to.home(), B_HOME, "and it is this machine's home");
    let can_go_back = switched_to.history().can_go_back();
    assert!(!can_go_back, "a machine just switched to has no history");
    ask(&mut browse, &b, &mut asked);

    let expected = (b.as_str().to_owned(), B_HOME.to_owned());
    let asked_first = asked.last().cloned();
    assert_eq!(asked_first, Some(expected), "the reset path is asked first");
    let named_the_left = asked.iter().any(|(_, path)| path == A_RESOLVED);
    assert!(
        !named_the_left,
        "no request names the left machine's resolved path"
    );

    // The machine left is untouched: the fence is per machine, so asking the new
    // one neither published nor cancelled anything over there.
    let left = browse.get(&a).expect("machine A is open");
    let standing = left.listing();
    assert_eq!(
        standing.resolved_path(),
        Some(A_RESOLVED),
        "A's path is A's"
    );
    assert_eq!(standing.entries().len(), 1, "and it kept its one row");
    assert_eq!(standing.entries()[0].name, "app", "which is A's own row");
}

/// A REPLY ADDRESSED TO THE MACHINE THE READER LEFT UPDATES THAT MACHINE AND
/// NOTHING ELSE. It lands late, while the reader is standing in the other
/// machine's directory, and it must not appear there under any spelling: not as
/// rows, and not as a resolved path a breadcrumb would paint.
#[test]
fn a_reply_for_the_machine_the_reader_left_cannot_repaint_the_one_it_moved_to() {
    let mut browse = BrowseState::new();
    let a = switch_to(&mut browse, WORKER_A, A_HOME);
    let mut asked = Vec::new();
    let late = ask(&mut browse, &a, &mut asked);
    let b = switch_to(&mut browse, WORKER_B, B_HOME);
    ask(&mut browse, &b, &mut asked);

    // Machine A's answer lands while the reader is browsing machine B.
    let rows_a = vec![BrowseEntry::dir("a-only", 1)];
    let answered = browse.apply_listing(&late, A_HOME.to_owned(), rows_a);
    assert!(answered, "the machine the reader left still answers");
    let machine_b = browse.get(&b).expect("machine B is open");
    let standing = machine_b.listing();
    assert!(standing.entries().is_empty(), "A's rows are not B's rows");
    assert_eq!(standing.resolved_path(), None, "nor is A's resolved path");

    // Machine B's own answer is what machine B shows.
    let for_b = ask(&mut browse, &b, &mut asked);
    let rows_b = vec![BrowseEntry::dir("b-only", 2)];
    let answered = browse.apply_listing(&for_b, B_HOME.to_owned(), rows_b);
    assert!(answered, "machine B's own answer stands");
    let shown = browse.get(&b).expect("machine B is open");
    assert_eq!(shown.listing().entries()[0].name, "b-only");
}

/// RETURNING TO A MACHINE DROPS THE GENERATION THE READER MOVED PAST. The
/// browser keeps the directory the reader left it in, so the fence rather than
/// the path is what stops the abandoned reply from repainting it.
#[test]
fn switching_back_drops_the_generation_the_reader_moved_past() {
    let mut browse = BrowseState::new();
    let a = switch_to(&mut browse, WORKER_A, A_HOME);
    let mut asked = Vec::new();
    ask(&mut browse, &a, &mut asked);
    let abandoned = ask(&mut browse, &a, &mut asked);

    let b = switch_to(&mut browse, WORKER_B, B_HOME);
    ask(&mut browse, &b, &mut asked);

    let reopened = browse.open(&a, Some(A_HOME));
    assert_eq!(
        reopened.cwd(),
        A_HOME,
        "the directory the reader left it in"
    );
    let current = ask(&mut browse, &a, &mut asked);
    assert_ne!(
        abandoned.generation, current.generation,
        "the fence moves on"
    );

    let rows = vec![BrowseEntry::dir("stale", 1)];
    let published = browse.apply_listing(&abandoned, A_HOME.to_owned(), rows);
    assert!(
        !published,
        "a reply from the generation moved past is dropped"
    );
    let standing = browse.get(&a).expect("machine A is open").listing();
    assert!(
        standing.entries().is_empty(),
        "so the old rows never published"
    );

    let rows = vec![BrowseEntry::dir("fresh", 2)];
    let published = browse.apply_listing(&current, A_HOME.to_owned(), rows);
    assert!(published, "the generation in force publishes");
    let standing = browse.get(&a).expect("machine A is open").listing();
    assert_eq!(standing.entries()[0].name, "fresh");
}

/// NO MACHINE IS EVER ASKED FOR ANOTHER MACHINE'S DIRECTORY. This is the whole
/// switch sequence the browser spec drives, asserted on the requests rather than
/// on the rows: the hazard is a request, because a request is what the
/// coordinator answers, and a wrong answer repaints under a breadcrumb naming
/// something else entirely.
#[test]
fn no_listing_is_ever_requested_against_another_machines_directory() {
    let mut browse = BrowseState::new();
    let a = switch_to(&mut browse, WORKER_A, A_HOME);
    let b = switch_to(&mut browse, WORKER_B, B_HOME);
    let mut asked: Vec<(String, String)> = Vec::new();

    ask(&mut browse, &a, &mut asked);
    browse.set_cwd(&a, A_DEEP);
    ask(&mut browse, &a, &mut asked);
    ask(&mut browse, &b, &mut asked);
    browse.set_cwd(&b, B_DEEP);
    ask(&mut browse, &b, &mut asked);
    browse.set_cwd(&a, A_HOME);
    ask(&mut browse, &a, &mut asked);

    let b_deep = (b.as_str().to_owned(), B_DEEP.to_owned());
    let a_on_b = (a.as_str().to_owned(), B_DEEP.to_owned());
    let b_on_a = (b.as_str().to_owned(), A_DEEP.to_owned());
    assert!(
        asked.contains(&b_deep),
        "the reader drilled into B: {asked:?}"
    );
    assert!(
        !asked.contains(&a_on_b),
        "machine A asked for B's path: {asked:?}"
    );
    assert!(
        !asked.contains(&b_on_a),
        "machine B asked for A's path: {asked:?}"
    );
}
