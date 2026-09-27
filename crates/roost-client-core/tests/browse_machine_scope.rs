//! Browse is a MACHINE-scoped view, and these tests are what keep it that way.
//!
//! Two machines can hold the same folder path — `~/src` on both is the ordinary
//! case, not an edge — so a browse cache keyed by path shows one machine's
//! folders under the other's name and nothing downstream can tell. The recents
//! list, the history, the filter and the listing fence are each keyed by
//! fingerprint here, and each test fails if one of them is not.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use roost_client_core::store::browse_entries::{
    BrowseEntry, entry_date, relative_entry_time, visible_files, visible_folders,
};
use roost_client_core::store::browse_machine::BrowseListing;
use roost_client_core::store::browse_paths::{BROWSE_HOME, BrowsePathOps, ExactBrowsePaths};
use roost_client_core::store::browse_state::BrowseState;
use roost_protocol::wire::{ChannelId, Session, SessionId, SessionKind, SessionStatus, WorkerFp};

const WORKER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn worker(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).expect("a worker fingerprint")
}

fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a session id")
}

fn session(worker_fp: &str, id: &str, cwd: &str, created_at: i64) -> Session {
    Session {
        id: session_id(id),
        worker_fp: worker(worker_fp),
        channel: ChannelId::try_from(1_i64).expect("a channel id"),
        kind: SessionKind::Shell,
        cwd: cwd.to_owned(),
        spawn_cwd: None,
        workspace_id: None,
        status: SessionStatus::Open,
        created_at,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

fn sessions() -> BTreeMap<SessionId, Session> {
    [
        session(
            WORKER_A,
            "10000000-0000-4000-8000-000000000001",
            "~/src",
            300,
        ),
        session(
            WORKER_A,
            "10000000-0000-4000-8000-000000000002",
            "/home/dev/work",
            200,
        ),
        // The same path, on the other machine. This row is the whole test.
        session(
            WORKER_B,
            "20000000-0000-4000-8000-000000000001",
            "~/src",
            900,
        ),
        session(
            WORKER_B,
            "20000000-0000-4000-8000-000000000002",
            "~/notes",
            800,
        ),
    ]
    .into_iter()
    .map(|row| (row.id.clone(), row))
    .collect()
}

#[test]
fn two_machines_at_the_same_path_hold_separate_browsers() {
    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    let b = worker(WORKER_B);

    // The same path on both machines, which is what a shared cache would merge.
    browse.open(&a, Some("~/src"));
    browse.open(&b, Some("~/src"));
    assert!(browse.open_child(&a, "app", &ExactBrowsePaths));
    assert!(browse.open_child(&b, "app", &ExactBrowsePaths));

    assert_eq!(
        browse.get(&a).expect("machine A is open").cwd(),
        "~/src/app"
    );
    assert_eq!(
        browse.get(&b).expect("machine B is open").cwd(),
        "~/src/app",
        "the two machines are at the same path, which is why only the KEY can \
         tell their listings apart"
    );
    assert_eq!(browse.len(), 2);

    browse.set_filter(&a, "ap");
    assert_eq!(browse.get(&a).expect("machine A is open").filter(), "ap");
    assert_eq!(
        browse.get(&b).expect("machine B is open").filter(),
        "",
        "a filter typed on one machine must not filter the other"
    );

    assert!(browse.go_back(&a));
    assert_eq!(browse.get(&a).expect("machine A is open").cwd(), "~/src");
    assert_eq!(
        browse.get(&b).expect("machine B is open").cwd(),
        "~/src/app",
        "one machine's Back must not move another machine"
    );
}

#[test]
fn a_machines_recents_never_include_another_machines_even_at_the_same_path() {
    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    let b = worker(WORKER_B);
    let all = sessions();
    browse.open(&a, None);
    browse.open(&b, None);

    let recents_a = browse.get(&a).expect("machine A is open").recents(&all);
    let recents_b = browse.get(&b).expect("machine B is open").recents(&all);

    assert_eq!(
        recents_a,
        vec!["~/src".to_owned(), "/home/dev/work".to_owned()]
    );
    assert_eq!(recents_b, vec!["~/src".to_owned(), "~/notes".to_owned()]);
    assert!(
        !recents_a.contains(&"~/notes".to_owned())
            && !recents_b.contains(&"/home/dev/work".to_owned()),
        "this test is only meaningful while the two lists differ"
    );
    assert_eq!(
        recents_a.iter().filter(|path| *path == "~/src").count(),
        1,
        "the newest session wins, and a duplicate path is listed once"
    );
}

#[test]
fn a_listing_reply_for_a_path_the_viewer_left_cannot_repaint_the_current_one() {
    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    browse.open(&a, Some("~/src"));

    let first = browse.begin_listing(&a).expect("machine A is open");
    assert!(matches!(
        browse.get(&a).expect("machine A is open").listing(),
        BrowseListing::Loading { .. }
    ));

    // The viewer moves on before the reply lands.
    browse.set_cwd(&a, "~/src/app");
    let second = browse.begin_listing(&a).expect("machine A is open");
    assert_ne!(
        first.generation, second.generation,
        "a second request must move the fence, or the first reply is not stale"
    );

    assert!(
        !browse.apply_listing(&first, "~/src".to_owned(), vec![BrowseEntry::dir("old", 1)]),
        "a reply for a directory the viewer has left must be dropped whole"
    );
    assert!(
        browse
            .get(&a)
            .expect("machine A is open")
            .listing()
            .entries()
            .is_empty()
    );

    assert!(browse.apply_listing(
        &second,
        "~/src/app".to_owned(),
        vec![BrowseEntry::dir("current", 2)],
    ));
    let listing = browse.get(&a).expect("machine A is open").listing();
    assert_eq!(listing.resolved_path(), Some("~/src/app"));
    assert_eq!(listing.entries().len(), 1);
    assert_eq!(listing.entries()[0].name, "current");
}

#[test]
fn two_machines_listing_at_once_do_not_fence_each_other() {
    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    let b = worker(WORKER_B);
    browse.open(&a, Some("~/src"));
    browse.open(&b, Some("~/src"));

    let request_a = browse.begin_listing(&a).expect("machine A is open");
    let request_b = browse.begin_listing(&b).expect("machine B is open");
    assert_eq!(
        request_a.generation, request_b.generation,
        "the fence is per machine, so two machines listing at once are two \
         independent generations that happen to be equal"
    );

    assert!(browse.apply_listing(
        &request_a,
        "~/src".to_owned(),
        vec![BrowseEntry::file("a-only", 1)],
    ));
    assert!(browse.apply_listing(
        &request_b,
        "~/src".to_owned(),
        vec![BrowseEntry::file("b-only", 1)],
    ));
    assert_eq!(
        browse
            .get(&a)
            .expect("machine A is open")
            .listing()
            .entries()[0]
            .name,
        "a-only"
    );
    assert_eq!(
        browse
            .get(&b)
            .expect("machine B is open")
            .listing()
            .entries()[0]
            .name,
        "b-only"
    );
}

#[test]
fn re_entering_a_machines_browser_keeps_the_directory_the_viewer_was_in() {
    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    browse.open(&a, Some("~/src"));
    browse.set_cwd(&a, "~/src/app");
    // The route is re-entered, and offers the machine's home again.
    let reopened = browse.open(&a, Some("~/src"));
    assert_eq!(
        reopened.cwd(),
        "~/src/app",
        "a route change must not reset a machine's browser to its home"
    );
    assert!(browse.forget(&a));
    assert!(browse.is_empty());
}

#[test]
fn a_filtered_listing_keeps_dot_directories_out_and_their_names_available_to_mkdir() {
    let entries = vec![
        BrowseEntry::dir("zeta", 10),
        BrowseEntry::dir("Alpha", 10),
        BrowseEntry::dir(".git", 10),
        BrowseEntry::file("beta.txt", 10),
        BrowseEntry::file("Alpha.md", 10),
    ];
    assert_eq!(
        visible_folders(&entries, "")
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<&str>>(),
        vec!["Alpha", "zeta"],
        "folders sort by name, case-insensitively, and dot-directories are hidden"
    );
    assert_eq!(
        visible_files(&entries, "alpha")
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<&str>>(),
        vec!["Alpha.md"]
    );
    assert_eq!(
        roost_client_core::store::browse_entries::folder_names(&entries),
        vec![".git", "Alpha", "zeta"],
        "a hidden directory's name is still taken, so mkdir must see it"
    );
}

#[test]
fn the_home_sentinel_is_a_path_browse_can_start_on_and_up_cannot_leave() {
    let paths = ExactBrowsePaths;
    // v2's `nativePathJoin` keeps the `~` sentinel and appends with a
    // separator, so a child of home is `~/src` — and one `..` from it lands
    // back on `~` rather than on `/`.
    assert_eq!(paths.child(BROWSE_HOME, "src"), "~/src");
    assert_eq!(paths.parent(BROWSE_HOME), BROWSE_HOME);
    assert_eq!(paths.parent("/"), "/");
    assert_eq!(paths.parent("/home/dev"), "/home");
    assert_eq!(paths.child("/home/dev", "src"), "/home/dev/src");

    let mut browse = BrowseState::new();
    let a = worker(WORKER_A);
    browse.open(&a, Some(BROWSE_HOME));
    assert!(!browse.get(&a).expect("machine A is open").can_go_up(&paths));
    assert!(!browse.go_up(&a, &paths), "up from home must not move");
    assert!(browse.open_child(&a, "src", &paths));
    assert!(browse.go_up(&a, &paths));
    assert_eq!(
        browse.get(&a).expect("machine A is open").cwd(),
        BROWSE_HOME
    );
}

#[test]
fn an_entry_time_reads_relative_and_then_as_a_utc_date() {
    let now = 1_800_000_000_000_i64;
    assert_eq!(relative_entry_time(0, now), "");
    assert_eq!(
        relative_entry_time(-1, now),
        "",
        "a clock behind reads as no time"
    );
    assert_eq!(relative_entry_time(now - 30_000, now), "just now");
    assert_eq!(relative_entry_time(now - 5 * 60_000, now), "5m ago");
    assert_eq!(relative_entry_time(now - 5 * 3_600_000, now), "5h ago");
    assert_eq!(relative_entry_time(now - 3 * 86_400_000, now), "3d ago");
    assert_eq!(entry_date(now - 400 * 86_400_000), "2025-12-11");
    assert_eq!(entry_date(0), "");
}
