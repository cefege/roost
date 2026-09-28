//! Inferred-link detection (`compute_row_links`): soft-wrap joining is decided
//! in grid COLUMNS, never code units; scheme URLs, `localhost:PORT` and GitHub
//! refs; and the edge matches of v2's regex battery. File paths, painted-link
//! precedence and gestures live in `terminal_links_{file_paths,precedence,gestures}.rs`.
//! Test names follow v2's `apps/web/tests/renderer/terminal-links.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_links_detect_support;

use roost_web_terminal::links::{RowLinkInput, compute_row_links};
use terminal_links_detect_support::{detect, reassemble, rows, stub_resolve, url_segment};

#[test]
fn url_wrapped_across_two_rows_is_one_link_with_two_per_row_segments() {
    let segments = detect(&["https://ex", "ample.com"], 10);
    assert_eq!(
        segments,
        vec![
            url_segment(0, 0, 10, "https://example.com"),
            url_segment(1, 0, 9, "https://example.com")
        ]
    );
}

#[test]
fn url_wrapped_across_three_rows_is_three_segments_with_the_same_href() {
    let texts = ["https://", "example.", "com/page"];
    let segments = detect(&texts, 8);
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.row)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(
        segments
            .iter()
            .all(|segment| segment.url == "https://example.com/page")
    );
    assert_eq!(reassemble(&texts, &segments), "https://example.com/page");
}

#[test]
fn without_a_grid_width_rows_are_never_joined() {
    assert!(
        detect(&["https://ex", "ample.com"], 0)
            .iter()
            .all(|segment| segment.row == 0)
    );
}

#[test]
fn a_single_row_url_keeps_its_offsets_within_the_row() {
    assert_eq!(
        detect(&["visit https://example.com now"], 80),
        vec![url_segment(0, 6, 25, "https://example.com")]
    );
}

#[test]
fn trailing_sentence_punctuation_is_excluded_from_a_wrapped_url() {
    let texts = ["https://ex", "ample.com."];
    assert_eq!(
        reassemble(&texts, &detect(&texts, 10)),
        "https://example.com"
    );
}

#[test]
fn a_short_line_that_does_not_fill_the_grid_is_not_joined_to_the_next() {
    assert_eq!(
        detect(&["hello", "https://example.com"], 10),
        vec![url_segment(1, 0, 19, "https://example.com")]
    );
}

#[test]
fn a_wide_glyph_row_that_fills_the_grid_still_joins() {
    let input = vec![
        RowLinkInput {
            text: "中中中http".to_string(),
            columns: 10,
            links: Vec::new(),
        },
        RowLinkInput {
            text: "://ex.co".to_string(),
            columns: 8,
            links: Vec::new(),
        },
    ];
    assert_eq!(
        input[0].text.encode_utf16().count(),
        7,
        "the count that would lose the link"
    );
    let segments = compute_row_links(&input, 10, None, None);
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.url.as_str())
            .collect::<Vec<_>>(),
        ["http://ex.co"; 2]
    );
    let texts = [input[0].text.as_str(), input[1].text.as_str()];
    assert_eq!(reassemble(&texts, &segments), "http://ex.co");
}

#[test]
fn an_emoji_cluster_row_that_does_not_fill_the_grid_must_not_join() {
    let cluster = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}x.ts";
    let input = vec![
        RowLinkInput {
            text: cluster.to_string(),
            columns: 6,
            links: Vec::new(),
        },
        RowLinkInput {
            text: ":9".to_string(),
            columns: 2,
            links: Vec::new(),
        },
    ];
    assert_eq!(
        cluster.encode_utf16().count(),
        15,
        "the count that would fabricate a join"
    );
    assert_eq!(
        compute_row_links(&input, 8, Some(&stub_resolve), None),
        vec![]
    );
    let as_units = rows(&[cluster, ":9"]);
    let bogus = compute_row_links(&as_units, 8, Some(&stub_resolve), None);
    assert!(bogus.iter().any(|segment| segment.file.is_some()));
}

#[test]
fn a_url_in_a_bordered_tui_panel_is_one_link_across_rows() {
    let texts = [
        "│ https://example.com/very/long/path/that/wraps│",
        "│ /across/rows/with/border/decoration│",
    ];
    let segments = detect(&texts, texts[0].encode_utf16().count() as i64);
    let url = "https://example.com/very/long/path/that/wraps/across/rows/with/border/decoration";
    assert!(segments.iter().all(|segment| segment.url == url));
    assert_eq!(reassemble(&texts, &segments), url);
}

#[test]
fn a_blank_bordered_row_between_two_urls_keeps_two_separate_links() {
    let first = "│ https://a.com/foo│";
    let width = first.encode_utf16().count();
    let blank = format!("│{}│", " ".repeat(width - 2));
    let segments = detect(&[first, &blank, "│ https://b.com/bar│"], width as i64);
    let mut urls: Vec<&str> = segments
        .iter()
        .map(|segment| segment.url.as_str())
        .collect();
    urls.dedup();
    assert_eq!(urls, ["https://a.com/foo", "https://b.com/bar"]);
}

#[test]
fn localhost_port_becomes_an_http_url_and_bare_localhost_does_not() {
    let segments = detect(&["Local: localhost:5174/app now"], 80);
    assert!(
        segments
            .iter()
            .any(|segment| segment.url == "http://localhost:5174/app")
    );
    assert_eq!(detect(&["run on localhost soon"], 80), vec![]);
}

#[test]
fn github_refs_resolve_self_contained_or_against_the_session_repo() {
    let urls = |text: &str, owner_repo: Option<&str>| -> Vec<String> {
        compute_row_links(&rows(&[text]), 80, None, owner_repo)
            .into_iter()
            .map(|segment| segment.url)
            .collect()
    };
    assert_eq!(
        urls("fixes owner/repo#412 today", None),
        ["https://github.com/owner/repo/issues/412"]
    );
    assert_eq!(
        urls("at owner/repo@deadbeef now", None),
        ["https://github.com/owner/repo/commit/deadbeef"]
    );
    assert_eq!(urls("see #7 and a1b2c3d4 here", None), Vec::<String>::new());
    assert_eq!(
        urls("see #7 and a1b2c3d4 here", Some("o/r")),
        [
            "https://github.com/o/r/issues/7",
            "https://github.com/o/r/commit/a1b2c3d4"
        ]
    );
    assert!(
        urls("build 1234567 done", Some("o/r"))
            .iter()
            .all(|url| !url.contains("/commit/"))
    );
}

#[test]
fn edge_matches_agree_with_the_v2_regex_battery() {
    // Expected values are v2 `computeRowLinks` output for the same input: the
    // `(?<![,.])` backtrack, bracketed suffixes, IPv6 ports, directory
    // backtracking, archive `\b`, and Windows drive/UNC paths with columns.
    let spans = |text: &str, files: bool| -> Vec<(usize, usize, String)> {
        let resolve = files
            .then_some(&stub_resolve as &dyn Fn(&str, Option<u64>, Option<&str>) -> Option<String>);
        compute_row_links(&rows(&[text]), 80, resolve, None)
            .into_iter()
            .map(|segment| (segment.start, segment.end, segment.url))
            .collect()
    };
    let owned = |expected: &[(usize, usize, &str)]| -> Vec<(usize, usize, String)> {
        expected
            .iter()
            .map(|(start, end, url)| (*start, *end, url.to_string()))
            .collect()
    };
    assert_eq!(
        spans("x data://.. y http://a.(b). z", false),
        owned(&[(14, 26, "http://a.(b)")])
    );
    assert_eq!(
        spans("open src/foo.ts/bar and ../a/b.tar.lz4 and x.zip-y", true),
        owned(&[
            (5, 15, "/file/W/src/foo.ts"),
            (24, 38, "/file/W/../a/b.tar.lz4"),
            (43, 48, "/file/W/x.zip")
        ])
    );
    assert_eq!(
        spans("http://[::1]:80., http://a,b,.; mailto:x@y.z", false),
        owned(&[(0, 15, "http://[::1]:80"), (18, 31, "http://a,b,.;")])
    );
    assert_eq!(
        spans(r"C:\a\b.ts:3:4 \\h\s\c.md:1", true),
        owned(&[
            (0, 13, r"/file/W/C:\a\b.ts#L3"),
            (14, 26, r"/file/W/\\h\s\c.md#L1")
        ])
    );
}
