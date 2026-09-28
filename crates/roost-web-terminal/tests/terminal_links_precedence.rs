//! Painted producer links win their span over inferred ones and are never
//! returned as work, including across a soft wrap; the link-hint prefilter
//! keeps every family detectable and skips a hint-free line. Test names
//! follow v2's `apps/web/tests/renderer/terminal-links.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_links_detect_support;

use roost_web_terminal::links::{PaintedLink, RowLinkInput, compute_row_links};
use terminal_links_detect_support::{PAINTED_KEY, detect, narrow, rows, stub_resolve, url_segment};

#[test]
fn a_painted_producer_uri_wins_its_span_over_the_identical_regex_url() {
    let text = "https://github.com/cefege/roost";
    let link = PaintedLink {
        start: 0,
        end: text.len(),
        uri: format!("{text}/?custom=1"),
        key: PAINTED_KEY.into(),
    };
    assert_eq!(
        compute_row_links(&[narrow(text, vec![link])], 80, None, None),
        vec![]
    );
    assert_eq!(
        detect(&[text], 80),
        vec![url_segment(0, 0, text.len(), text)]
    );
}

#[test]
fn a_valid_painted_file_target_stays_authoritative_including_across_a_soft_wrap() {
    let text = "/Users/you/Code/project/docs/report_2026-07-13.zip";
    let at = text.find("report_2026-07-13.zip").unwrap();
    let link = PaintedLink {
        start: at,
        end: text.len(),
        uri: format!("file://{text}"),
        key: PAINTED_KEY.into(),
    };
    assert_eq!(
        compute_row_links(&[narrow(text, vec![link])], 80, Some(&stub_resolve), None),
        vec![]
    );
    let half = |start, end| PaintedLink {
        start,
        end,
        uri: "file:///tmp/Foo.txt".into(),
        key: PAINTED_KEY.into(),
    };
    let wrapped = vec![
        RowLinkInput {
            text: "see /tmp/F".into(),
            columns: 10,
            links: vec![half(9, 10)],
        },
        RowLinkInput {
            text: "oo.txt".into(),
            columns: 6,
            links: vec![half(0, 6)],
        },
    ];
    assert_eq!(
        compute_row_links(&wrapped, 10, Some(&stub_resolve), None),
        vec![]
    );
}

#[test]
fn the_prefilter_keeps_every_link_family_detectable() {
    let resolve = |path: &str, line: Option<u64>, _: Option<&str>| {
        Some(format!(
            "/file/fp/{path}{}",
            line.map_or(String::new(), |line| format!("#L{line}"))
        ))
    };
    let one = |text: &str, owner_repo: Option<&str>| -> Vec<String> {
        compute_row_links(&rows(&[text]), 80, Some(&resolve), owner_repo)
            .into_iter()
            .map(|segment| segment.url)
            .collect()
    };
    assert_eq!(
        one("open https://example.com now", None),
        ["https://example.com"]
    );
    assert_eq!(one("mail me at mailto:a@b.co", None), Vec::<String>::new());
    assert_eq!(
        one("Local:   localhost:5174/", None),
        ["http://localhost:5174/"]
    );
    assert_eq!(
        one("at apps/web/src/foo.ts:42 exactly", None),
        ["/file/fp/apps/web/src/foo.ts#L42"]
    );
    assert_eq!(one("see foo.ts:9 there", None), ["/file/fp/foo.ts#L9"]);
    assert_eq!(
        one("grab release.tar.gz please", None),
        ["/file/fp/release.tar.gz"]
    );
    assert_eq!(
        one("fixed cefege/roost#12 today", None),
        ["https://github.com/cefege/roost/issues/12"]
    );
    assert_eq!(
        one("landed cefege/roost@deadbeef ok", None),
        ["https://github.com/cefege/roost/commit/deadbeef"]
    );
    assert_eq!(
        one("closes #77 finally", Some("cefege/roost")),
        ["https://github.com/cefege/roost/issues/77"]
    );
    assert_eq!(
        one("reverted deadbeef1 earlier", Some("cefege/roost")),
        ["https://github.com/cefege/roost/commit/deadbeef1"]
    );
    assert_eq!(
        one(
            "the quick brown fox jumps over a lazy dog",
            Some("cefege/roost")
        ),
        Vec::<String>::new()
    );
}

#[test]
fn the_prefilter_skips_a_hint_free_line_and_leaves_its_painted_anchor_alone() {
    let link = PaintedLink {
        start: 5,
        end: 11,
        uri: "https://x.test/readme".into(),
        key: PAINTED_KEY.into(),
    };
    assert_eq!(
        compute_row_links(&[narrow("open Readme now", vec![link])], 80, None, None),
        vec![]
    );
}
