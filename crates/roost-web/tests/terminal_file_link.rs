//! The terminal file-link transition: a path a process printed becomes an
//! `a.wterm-link[data-kind="file"]` anchor only when the pane's resolver mints a
//! `/file/…` route for it, and the anchor's text is the path exactly as it was
//! printed. Target-independent — the detector, the resolver and the path codec
//! all run natively, and the browser only supplies the DOM the segment wraps.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::platform::worker_paths::resolve_worker_path;
use roost_web::routes::Route;
use roost_web::terminal_file_link::resolve_terminal_file;
use roost_web_terminal::link_target::ResolveFile;
use roost_web_terminal::links::{RowLinkInput, RowLinkSegment, compute_row_links};

const WORKER_FP: &str = "aa11bb22";
const CWD: &str = "/tmp/roost-fixture/home";
/// The row the mobile keyboard spec emits before it looks for the anchor.
const PRINTED_ROW: &str = "ROOST_PTY_READY/1 s/mobile-link.ts:9";
const PRINTED_TARGET: &str = "s/mobile-link.ts:9";

/// The resolver a mounted pane installs, bound to this session.
fn pane_resolver(
    raw_path: &str,
    line: Option<u64>,
    file_authority: Option<&str>,
) -> Option<String> {
    resolve_terminal_file(
        Some("linux"),
        WORKER_FP,
        CWD,
        raw_path,
        line,
        file_authority,
    )
}

fn segments_of(text: &str, resolve: Option<ResolveFile<'_>>) -> Vec<RowLinkSegment> {
    let row = RowLinkInput {
        text: text.to_owned(),
        columns: 80,
        links: Vec::new(),
    };
    compute_row_links(&[row], 80, resolve, None)
}

fn file_links_of(text: &str, resolve: Option<ResolveFile<'_>>) -> Vec<(String, String)> {
    segments_of(text, resolve)
        .into_iter()
        .filter_map(|segment| {
            let file = segment.file?;
            Some((file.source, segment.url))
        })
        .collect()
}

#[test]
fn a_printed_path_becomes_one_file_link_carrying_its_line() {
    assert_eq!(
        file_links_of(PRINTED_ROW, Some(&pane_resolver)),
        vec![(
            PRINTED_TARGET.to_owned(),
            format!("/file/{WORKER_FP}/tmp/roost-fixture/home/s/mobile-link.ts#L9"),
        )]
    );
}

#[test]
fn the_anchor_text_is_the_path_exactly_as_it_was_printed() {
    let segments = segments_of(PRINTED_ROW, Some(&pane_resolver));
    let wrapped = &PRINTED_ROW[segments[0].start..segments[0].end];
    let source = segments[0]
        .file
        .as_ref()
        .map(|file| file.source.as_str())
        .unwrap_or("");
    assert_eq!(wrapped, source);
}

#[test]
fn the_same_row_offers_no_file_link_without_a_resolver() {
    // The gate that let the anchor go missing: the file battery never runs
    // without a resolver, so the path stayed the plain text it arrived as.
    assert_eq!(file_links_of(PRINTED_ROW, None), Vec::new());
    assert_eq!(segments_of(PRINTED_ROW, None), Vec::new());
}

#[test]
fn a_minted_file_href_is_the_route_the_shell_will_render() {
    // The detector splits a printed `:line` off before it calls the resolver,
    // so the resolver is handed the path and the line, never the raw text.
    let href = pane_resolver("s/mobile-link.ts", Some(9), None).unwrap_or_default();
    // The shell routes on pathname and search and never on the fragment, so the
    // line a process printed is a fragment and the path beside it is a route.
    let (route_path, fragment) = href
        .split_once('#')
        .unwrap_or_else(|| panic!("a file link carries its line as a fragment: {href}"));
    assert_eq!(fragment, "L9");
    assert_eq!(
        Route::parse(route_path),
        Route::File {
            worker_fp: WORKER_FP.to_owned(),
            path: "tmp/roost-fixture/home/s/mobile-link.ts".to_owned(),
        }
    );
}

#[test]
fn text_the_renderer_was_not_told_is_a_file_never_becomes_one() {
    for row in [
        "mailto:someone@example.test",
        "ROOST_PTY_READY/1",
        "opened 12 files",
    ] {
        assert_eq!(
            file_links_of(row, Some(&pane_resolver)),
            Vec::new(),
            "row was {row}"
        );
    }
}

#[test]
fn a_home_relative_path_resolves_against_the_workers_own_home() {
    assert_eq!(
        resolve_worker_path(Some("linux"), "/home/me/proj", "~/notes/a.ts").as_deref(),
        Some("/home/me/notes/a.ts")
    );
    // A folder with no home to expand against refuses rather than guessing.
    assert_eq!(resolve_worker_path(Some("linux"), "/srv", "~/notes"), None);
}

#[test]
fn a_windows_drive_relative_path_is_refused_and_a_drive_absolute_one_is_not() {
    let folder = r"C:\Users\me\proj";
    // `C:proj` means "proj under whatever that drive was sitting in", which is
    // state this browser cannot see.
    assert_eq!(
        resolve_worker_path(Some("win32"), folder, r"C:proj\a.ts"),
        None
    );
    assert_eq!(
        resolve_worker_path(Some("win32"), folder, r"C:\other\a.ts").as_deref(),
        Some("C:/other/a.ts")
    );
    // A rooted path with no drive is rooted on the folder's own drive.
    assert_eq!(
        resolve_worker_path(Some("win32"), folder, "/a/b.ts").as_deref(),
        Some("C:/a/b.ts")
    );
}

#[test]
fn a_protocol_relative_target_is_only_a_path_on_windows() {
    // `//host/share` is ambiguous with a protocol-relative URL, so a POSIX
    // worker must refuse it rather than read a route as a filename.
    assert_eq!(
        resolve_terminal_file(
            Some("linux"),
            WORKER_FP,
            CWD,
            "//host/share/a.ts",
            None,
            None
        ),
        None
    );
    assert_eq!(
        resolve_terminal_file(
            Some("win32"),
            WORKER_FP,
            r"C:\Users\me\proj",
            r"\\host\share\a.ts",
            Some(3),
            None
        )
        .as_deref(),
        Some("/file/aa11bb22/~unc/host/share/a.ts#L3")
    );
}
