//! The `/file/…` route's whole trip, from the href a producer mints to the
//! absolute path a worker will accept. The route grammar owns the shape in
//! between, so a producer that mints one shape and a viewer that reads another
//! is invisible at every call site and fatal at the worker, which refuses any
//! browser path that is not absolute.
//!
//! Target-independent: the linkifier, the browse row builder, `Route::parse`
//! and the path codec are pure, and "the path a worker recognises" is the
//! shared `roost-platform` normalizer the worker's own file commands gate on —
//! not a live machine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_platform::{HostPlatform, native_path_to_fs_path, normalize_native_path};

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::{Mutation, Mutations};
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_web::components::file_viewer::FileViewer;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::router_state::provide_router;

use roost_web::Route;
use roost_web::terminal_file_link::resolve_terminal_file;
use roost_web::terminal_href::{child_file_href, file_target};

const WORKER_FP: &str = "aa11bb22cc33dd44";
const CWD: &str = "/home/mike/project";

/// The trip the file viewer takes: parse the route, then ask what path its
/// splat names on that machine. `None` is what the sheet renders nothing for.
fn what_the_viewer_would_read(href: &str, worker_os: Option<&str>) -> Option<String> {
    let route_path = href.split('#').next().unwrap_or(href);
    file_target(worker_os, &Route::parse(route_path)).map(|(_worker_fp, path)| path)
}

/// The worker's own admission rule for a browser-supplied path, from
/// `roost-worker`'s file commands: normalize, refuse anything that is not
/// rooted, then convert to a filesystem path. Asserted through the shared codec
/// so a path this test calls readable is a path the machine reads the same way.
fn the_worker_would_open(platform: HostPlatform, path: &str) -> bool {
    normalize_native_path(platform, path)
        .ok()
        .filter(|normalized| normalized.starts_with('/'))
        .and_then(|normalized| native_path_to_fs_path(platform, &normalized).ok())
        .is_some()
}

#[test]
fn a_printed_path_is_read_back_as_the_printed_path() {
    let href = resolve_terminal_file(Some("linux"), WORKER_FP, CWD, "src/main.rs", Some(42), None)
        .expect("a printed path on a live POSIX worker mints a route");
    // The line a process printed is a fragment; the shell routes on the path.
    assert_eq!(
        href,
        format!("/file/{WORKER_FP}/home/mike/project/src/main.rs#L42")
    );

    let read =
        what_the_viewer_would_read(&href, Some("linux")).expect("the viewer decodes its own route");
    assert_eq!(read, "/home/mike/project/src/main.rs");
    assert!(the_worker_would_open(HostPlatform::Linux, &read));
}

#[test]
fn the_route_splat_is_the_route_and_not_the_path() {
    let href = resolve_terminal_file(Some("linux"), WORKER_FP, CWD, "src/main.rs", None, None)
        .expect("a printed path mints a route");
    let route_path = href.split('#').next().unwrap_or_default();
    // The grammar's own round trip: a link built from a route and a link typed
    // by a reader are the same route, which only holds while `path` is the
    // splat rather than a decoded native path.
    let route = Route::parse(route_path);
    assert_eq!(
        route.to_path(),
        route_path,
        "parse must hand back the splat it was given, or to_path is not its inverse"
    );
}

#[test]
fn a_name_with_a_space_and_a_percent_is_not_unescaped_twice() {
    // `%` is not an unreserved byte, so the route carries it as `%25` and the
    // space as `%20`. Unescaping it in the grammar and again in the path codec
    // would leave the viewer holding a `%` the codec refuses.
    let printed = resolve_terminal_file(Some("linux"), WORKER_FP, CWD, "100% done.txt", None, None)
        .expect("a printed POSIX name mints a route");
    assert_eq!(
        printed,
        format!("/file/{WORKER_FP}/home/mike/project/100%25%20done.txt")
    );
    assert_eq!(
        what_the_viewer_would_read(&printed, Some("linux")).as_deref(),
        Some("/home/mike/project/100% done.txt")
    );

    let listed = child_file_href(Some("linux"), WORKER_FP, CWD, "100% done.txt")
        .expect("a listed name mints a route");
    assert_eq!(listed, printed);
    assert_eq!(
        what_the_viewer_would_read(&listed, Some("linux")).as_deref(),
        Some("/home/mike/project/100% done.txt")
    );
}

#[test]
fn a_windows_drive_path_keeps_its_tagged_root_through_the_route() {
    let href = resolve_terminal_file(
        Some("win32"),
        WORKER_FP,
        "C:\\src",
        "main.ts",
        Some(7),
        None,
    )
    .expect("a drive-absolute Windows path mints a route");
    assert_eq!(href, format!("/file/{WORKER_FP}/~drive/C/src/main.ts#L7"));
    assert_eq!(
        what_the_viewer_would_read(&href, Some("win32")).as_deref(),
        Some("C:/src/main.ts"),
        "a Windows route is unreadable without its tagged root"
    );
}

#[test]
fn a_listed_file_and_a_printed_path_address_one_file_the_same_way() {
    for (worker_os, dir, name) in [
        ("linux", CWD, "README.md"),
        ("win32", "C:\\src", "notes.md"),
    ] {
        let listed = child_file_href(Some(worker_os), WORKER_FP, dir, name)
            .expect("listed file mints a route");
        let printed = resolve_terminal_file(Some(worker_os), WORKER_FP, dir, name, None, None)
            .expect("a printed file mints a route");
        assert_eq!(
            listed, printed,
            "{worker_os}: the browse tree and the terminal linkifier mint different routes"
        );
    }
}

#[test]
fn a_path_this_product_cannot_address_gets_no_href_at_all() {
    // An OS the codec has no rules for is not a file this product can open, and
    // a row whose href names one is a link that opens nothing.
    assert_eq!(
        child_file_href(Some("solaris"), WORKER_FP, CWD, "README.md"),
        None
    );
    assert_eq!(
        resolve_terminal_file(Some("solaris"), WORKER_FP, CWD, "README.md", None, None),
        None
    );
}

#[test]
fn a_route_this_product_cannot_address_opens_nothing() {
    // A machine advertising an OS the codec has no rules for names no file, so
    // the sheet shows nothing rather than sending the worker a path to guess at.
    let route = Route::File {
        worker_fp: WORKER_FP.to_owned(),
        path: "home/mike/project/notes.md".to_owned(),
    };
    assert_eq!(file_target(Some("solaris"), &route), None);
    assert_eq!(file_target(None, &Route::Home), None);
    assert_eq!(
        file_target(Some("linux"), &route).map(|(_fp, path)| path),
        Some("/home/mike/project/notes.md".to_owned())
    );
}

thread_local! {
    /// The route the mount is rendering, handed to the root `VirtualDom::new`
    /// wants as a plain function pointer.
    static MOUNTED_ROUTE: RefCell<Option<Route>> = const { RefCell::new(None) };
}

/// The root, with every context the sheet reads.
fn viewer_root() -> Element {
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("file-route"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    provide_router(
        Signal::new(String::new()),
        EventHandler::new(|_path: String| {}),
    );
    let route = MOUNTED_ROUTE.with(|mounted| mounted.borrow_mut().take());
    rsx! { FileViewer { route: route.unwrap_or(Route::Home) } }
}

/// Mount the real sheet over the href a real producer minted, and read back the
/// text it paints. This is the consumer half: the header's path and the path
/// the read is issued for are one binding, so what the sheet names is what the
/// worker is asked for — and a route the sheet paints as its own splat is a
/// route whose read the worker refuses as relative.
fn painted_paths(href: &str) -> Vec<String> {
    MOUNTED_ROUTE.with(|mounted| *mounted.borrow_mut() = Some(Route::parse(href)));
    let mut dom = VirtualDom::new(viewer_root);
    let mut painted: Vec<String> = Vec::new();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    collect_text(&initial, &mut painted);
    for _ in 0..SETTLE_ROUNDS {
        collect_text(&dom.render_immediate_to_vec(), &mut painted);
        dom.process_events();
    }
    painted
}

/// Every string one pass put on screen, which is what a browser would paint.
fn collect_text(mutations: &Mutations, painted: &mut Vec<String>) {
    for edit in &mutations.edits {
        match edit {
            Mutation::CreateTextNode { value, .. } => painted.push(value.clone()),
            Mutation::SetText { value, .. } => painted.push(value.clone()),
            _ => {}
        }
    }
}

/// How many render-then-effect rounds one mutation is given, so a memo that
/// recomputes asynchronously has published before the assertion reads the tree.
const SETTLE_ROUNDS: usize = 4;

#[test]
fn the_sheet_paints_the_path_a_worker_will_open_and_not_the_route_splat() {
    for (href, absolute) in [
        (
            format!("/file/{WORKER_FP}/home/mike/project/src/main.rs"),
            "/home/mike/project/src/main.rs".to_owned(),
        ),
        (
            format!("/file/{WORKER_FP}/~drive/C/src/main.ts"),
            "C:/src/main.ts".to_owned(),
        ),
    ] {
        let painted = painted_paths(&href);
        assert!(
            painted.contains(&absolute),
            "{href}: the sheet painted {painted:?} and never named the absolute path the \
             read is issued for"
        );
    }
}
