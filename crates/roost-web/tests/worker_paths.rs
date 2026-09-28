//! The browser path codec and the browse palette over it: v2
//! `apps/web/tests/folderPalette.test.ts`, plus the identity and display rules
//! of `apps/web/src/lib/nativePath.ts` that the sidebar's folder keys and
//! labels depend on.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::store::WorkerPaths;
use roost_web::platform::worker_paths::palette::{
    CrumbView, child_path, collapse_crumbs_to, parent_path, path_crumbs,
};
use roost_web::platform::worker_paths::{
    BrowserWorkerPaths, WorkerPathCrumb, short_worker_path, worker_path_platform,
};

fn crumb(label: &str, path: &str) -> WorkerPathCrumb {
    WorkerPathCrumb {
        label: label.to_owned(),
        path: path.to_owned(),
    }
}

fn shown(label: &str, path: &str) -> CrumbView {
    CrumbView::Crumb(crumb(label, path))
}

#[test]
fn child_path_joins_handling_root_and_trailing_slash() {
    assert_eq!(
        child_path(None, "~/code", "roost").as_deref(),
        Some("~/code/roost")
    );
    assert_eq!(
        child_path(None, "~/code/", "roost").as_deref(),
        Some("~/code/roost")
    );
    assert_eq!(child_path(None, "/", "usr").as_deref(), Some("/usr"));
}

#[test]
fn path_crumbs_are_cumulative_for_absolute_and_home_roots() {
    assert_eq!(
        path_crumbs(None, "/Users/you"),
        [
            crumb("/", "/"),
            crumb("Users", "/Users"),
            crumb("you", "/Users/you")
        ]
    );
    assert_eq!(
        path_crumbs(None, "~/Code/roost"),
        [
            crumb("~", "~"),
            crumb("Code", "~/Code"),
            crumb("roost", "~/Code/roost")
        ]
    );
    assert_eq!(path_crumbs(None, ""), []);
}

#[test]
fn parent_path_drops_the_last_segment_and_roots_stay_put() {
    assert_eq!(parent_path(None, "/Users/you"), "/Users");
    assert_eq!(parent_path(None, "~/Code"), "~");
    assert_eq!(parent_path(None, "~"), "~");
    assert_eq!(parent_path(None, "/"), "/");
}

#[test]
fn windows_drive_and_unc_paths_keep_their_native_roots() {
    assert_eq!(parent_path(None, "C:/Users/Ada"), "C:/Users");
    assert_eq!(
        child_path(None, "//server/share", "build").as_deref(),
        Some("//server/share/build")
    );
    let trail = path_crumbs(None, "C:/Users/Ada/Code");
    assert_eq!(
        trail[1..],
        [
            crumb("Users", "C:/Users"),
            crumb("Ada", "C:/Users/Ada"),
            crumb("Code", "C:/Users/Ada/Code"),
        ]
    );
    assert_eq!(trail[0].path, "C:/");
}

#[test]
fn a_windows_drive_crumb_reads_as_the_bare_drive() {
    assert_eq!(
        path_crumbs(None, "C:/Users/Ada/Code")[0],
        crumb("C:", "C:/")
    );
}

#[test]
fn collapse_with_nothing_hidden_returns_every_crumb() {
    let crumbs = path_crumbs(None, "/a/b/c/d/e");
    assert_eq!(
        collapse_crumbs_to(&crumbs, 0),
        [
            shown("/", "/"),
            shown("a", "/a"),
            shown("b", "/a/b"),
            shown("c", "/a/b/c"),
            shown("d", "/a/b/c/d"),
            shown("e", "/a/b/c/d/e"),
        ]
    );
}

#[test]
fn collapse_folds_from_the_left_of_the_middle_and_keeps_parent_and_current() {
    let crumbs = path_crumbs(None, "/a/b/c/d/e");
    assert_eq!(
        collapse_crumbs_to(&crumbs, 2),
        [
            shown("/", "/"),
            CrumbView::Ellipsis(vec![crumb("a", "/a"), crumb("b", "/a/b")]),
            shown("c", "/a/b/c"),
            shown("d", "/a/b/c/d"),
            shown("e", "/a/b/c/d/e"),
        ]
    );
    assert_eq!(
        collapse_crumbs_to(&crumbs, 3),
        [
            shown("/", "/"),
            CrumbView::Ellipsis(vec![
                crumb("a", "/a"),
                crumb("b", "/a/b"),
                crumb("c", "/a/b/c")
            ]),
            shown("d", "/a/b/c/d"),
            shown("e", "/a/b/c/d/e"),
        ]
    );
}

#[test]
fn three_crumbs_never_collapse_and_hide_middle_is_clamped() {
    assert_eq!(
        collapse_crumbs_to(&path_crumbs(None, "/a/b"), 5),
        [shown("/", "/"), shown("a", "/a"), shown("b", "/a/b")]
    );
    let deep = path_crumbs(None, "/a/b/c/d/e");
    assert_eq!(collapse_crumbs_to(&deep, 99), collapse_crumbs_to(&deep, 3));
}

#[test]
fn a_declared_platform_wins_and_an_unknown_one_is_refused() {
    let paths = BrowserWorkerPaths;
    // Windows folds case; the same spelling on Linux is two folders.
    assert_eq!(
        paths.folder_key(Some("win32"), "C:\\Work\\Proj"),
        paths.folder_key(Some("win32"), "c:/work/proj")
    );
    assert_ne!(
        paths.folder_key(Some("linux"), "/Work"),
        paths.folder_key(Some("linux"), "/work")
    );
    // macOS `/tmp` IS `/private/tmp`.
    assert_eq!(
        paths.folder_key(Some("darwin"), "/tmp/x"),
        paths.folder_key(Some("darwin"), "/private/tmp/x")
    );
    assert_eq!(worker_path_platform(Some("plan9"), "/x"), None);
    assert_eq!(paths.folder_key(Some("plan9"), "/x"), None);
}

#[test]
fn an_unhydrated_worker_infers_windows_only_from_windows_spellings() {
    let paths = BrowserWorkerPaths;
    assert_eq!(
        paths.folder_key(None, "C:/Work"),
        paths.folder_key(None, "c:\\work")
    );
    assert_eq!(
        paths.folder_key(None, "//Server/Share/A"),
        paths.folder_key(None, "\\\\server\\share\\a")
    );
    assert_ne!(
        paths.folder_key(None, "/Work"),
        paths.folder_key(None, "/work")
    );
    assert_eq!(
        paths.basename(None, "/srv/roost/apps/web/").as_deref(),
        Some("web")
    );
}

#[test]
fn short_worker_path_reads_user_and_basename_under_a_home_root() {
    assert_eq!(
        short_worker_path(None, "/Users/you/Code/roost"),
        "you/roost"
    );
    assert_eq!(short_worker_path(None, "/home/user/repos/foo"), "user/foo");
    assert_eq!(short_worker_path(None, "C:/Users/you/roost"), "you/roost");
    assert_eq!(short_worker_path(None, "/home/user"), "user");
    assert_eq!(short_worker_path(None, "/srv/app"), "app");
    assert_eq!(short_worker_path(Some("linux"), "relative/x"), "relative/x");
}

fn activity(cwds: &[(&str, &str)], folders: &[&str]) -> Vec<(String, usize)> {
    use roost_client_core::store::folder_activity::compute_folder_activity;
    use roost_protocol::wire::{
        ChannelId, Session, SessionId, SessionKind, SessionStatus, WorkerFp,
    };
    let rows: Vec<Session> = cwds
        .iter()
        .enumerate()
        .map(|(index, (fp, cwd))| Session {
            id: SessionId::try_from(format!("00000000-0000-4000-8000-00000000000{index}"))
                .expect("an id"),
            worker_fp: WorkerFp::try_from((*fp).to_owned()).expect("a fingerprint"),
            channel: ChannelId::try_from(1_i64).expect("a channel"),
            kind: SessionKind::Shell,
            cwd: (*cwd).to_owned(),
            spawn_cwd: Some((*cwd).to_owned()),
            workspace_id: None,
            status: SessionStatus::Open,
            created_at: 1_000,
            closed_at: None,
            custom_title: None,
            git_branch: None,
            git_remote: None,
            pr_number: None,
            pr_state: None,
            pr_checks: None,
            pr_url: None,
            ports: None,
        })
        .collect();
    let refs: Vec<&Session> = rows.iter().collect();
    compute_folder_activity(&refs, &BrowserWorkerPaths, None, MACHINE, folders)
        .into_iter()
        .map(|(path, counted)| (path, counted.terminals))
        .collect()
}

const MACHINE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_MACHINE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn counted(path: &str, terminals: usize) -> Vec<(String, usize)> {
    vec![(path.to_owned(), terminals)]
}

#[test]
fn folder_activity_is_empty_without_a_session_inside() {
    assert!(activity(&[], &[]).is_empty());
    assert!(activity(&[], &["/a"]).is_empty());
    assert!(activity(&[(MACHINE, "/other")], &["/a", "/b"]).is_empty());
}

#[test]
fn folder_activity_counts_a_folder_and_its_whole_subtree() {
    assert_eq!(
        activity(&[(MACHINE, "/a")], &["/a", "/b"]),
        counted("/a", 1)
    );
    assert_eq!(activity(&[(MACHINE, "/a/b/c")], &["/a"]), counted("/a", 1));
    assert_eq!(
        activity(
            &[(MACHINE, "/a"), (MACHINE, "/a/b"), (MACHINE, "/a/c/d")],
            &["/a"]
        ),
        counted("/a", 3)
    );
    assert_eq!(
        activity(&[(MACHINE, "/a"), (OTHER_MACHINE, "/a")], &["/a"]),
        counted("/a", 1)
    );
}

#[test]
fn folder_activity_handles_trailing_slash_root_and_home() {
    assert_eq!(activity(&[(MACHINE, "/a")], &["/a/"]), counted("/a/", 1));
    assert_eq!(
        activity(
            &[(MACHINE, "/"), (MACHINE, "/a"), (MACHINE, "/a/b")],
            &["/"]
        ),
        counted("/", 3)
    );
    assert_eq!(activity(&[(MACHINE, "~/a")], &["~"]), counted("~", 1));
}

#[test]
fn folder_activity_folds_windows_case_at_segment_boundaries() {
    assert_eq!(
        activity(
            &[
                (MACHINE, "c:/work/project/src"),
                (MACHINE, "C:/WORKER/not-a-child")
            ],
            &["C:/Work/Project"]
        ),
        counted("C:/Work/Project", 1)
    );
}
