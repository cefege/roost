//! Inferred-link detection (`compute_row_links`) and the link gesture rules.
//! Soft-wrap joining is decided in grid COLUMNS, never code units; inferred
//! links (regex URLs, GitHub refs, resolvable paths) yield to PAINTED producer
//! links, which are never returned as work. Test names follow v2's
//! `apps/web/tests/renderer/terminal-links.test.ts` and `terminal-links.dom.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web_terminal::links::{
    LinkActivationGesture, LinkModifierKey, PaintedLink, PressWithheld, RowLinkInput,
    RowLinkSegment, compute_row_links, is_link_activation_gesture, is_link_modifier_held,
    withhold_press,
};

/// Rows of NARROW characters: one column per UTF-16 code unit.
fn rows(texts: &[&str]) -> Vec<RowLinkInput> {
    texts.iter().map(|text| narrow(text, Vec::new())).collect()
}

fn narrow(text: &str, links: Vec<PaintedLink>) -> RowLinkInput {
    RowLinkInput { text: text.to_string(), columns: text.encode_utf16().count() as i64, links }
}

fn stub_resolve(path: &str, line: Option<u64>, _authority: Option<&str>) -> Option<String> {
    let path = path.strip_prefix('/').unwrap_or(path);
    Some(match line {
        Some(line) => format!("/file/W/{path}#L{line}"),
        None => format!("/file/W/{path}"),
    })
}

fn detect(texts: &[&str], cols: i64) -> Vec<RowLinkSegment> {
    compute_row_links(&rows(texts), cols, None, None)
}

fn detect_files(texts: &[&str]) -> Vec<RowLinkSegment> {
    compute_row_links(&rows(texts), 80, Some(&stub_resolve), None)
}

fn has_file(texts: &[&str]) -> bool {
    detect_files(texts).iter().any(|segment| segment.file.is_some())
}

/// JS `text.slice(start, end)` by UTF-16 code unit.
fn slice(text: &str, start: usize, end: usize) -> String {
    String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()[start..end]).unwrap()
}

fn reassemble(texts: &[&str], segments: &[RowLinkSegment]) -> String {
    segments.iter().map(|segment| slice(texts[segment.row], segment.start, segment.end)).collect()
}

fn url_segment(row: usize, start: usize, end: usize, url: &str) -> RowLinkSegment {
    RowLinkSegment { row, start, end, url: url.to_string(), file: None }
}

const PAINTED_KEY: &str = "b\u{0}7";

#[test]
fn url_wrapped_across_two_rows_is_one_link_with_two_per_row_segments() {
    let segments = detect(&["https://ex", "ample.com"], 10);
    assert_eq!(
        segments,
        vec![url_segment(0, 0, 10, "https://example.com"), url_segment(1, 0, 9, "https://example.com")]
    );
}

#[test]
fn url_wrapped_across_three_rows_is_three_segments_with_the_same_href() {
    let texts = ["https://", "example.", "com/page"];
    let segments = detect(&texts, 8);
    assert_eq!(segments.iter().map(|segment| segment.row).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(segments.iter().all(|segment| segment.url == "https://example.com/page"));
    assert_eq!(reassemble(&texts, &segments), "https://example.com/page");
}

#[test]
fn without_a_grid_width_rows_are_never_joined() {
    assert!(detect(&["https://ex", "ample.com"], 0).iter().all(|segment| segment.row == 0));
}

#[test]
fn a_single_row_url_keeps_its_offsets_within_the_row() {
    assert_eq!(detect(&["visit https://example.com now"], 80), vec![url_segment(0, 6, 25, "https://example.com")]);
}

#[test]
fn trailing_sentence_punctuation_is_excluded_from_a_wrapped_url() {
    let texts = ["https://ex", "ample.com."];
    assert_eq!(reassemble(&texts, &detect(&texts, 10)), "https://example.com");
}

#[test]
fn a_short_line_that_does_not_fill_the_grid_is_not_joined_to_the_next() {
    assert_eq!(detect(&["hello", "https://example.com"], 10), vec![url_segment(1, 0, 19, "https://example.com")]);
}

#[test]
fn a_wide_glyph_row_that_fills_the_grid_still_joins() {
    let input = vec![
        RowLinkInput { text: "中中中http".to_string(), columns: 10, links: Vec::new() },
        RowLinkInput { text: "://ex.co".to_string(), columns: 8, links: Vec::new() },
    ];
    assert_eq!(input[0].text.encode_utf16().count(), 7, "the count that would lose the link");
    let segments = compute_row_links(&input, 10, None, None);
    assert_eq!(segments.iter().map(|segment| segment.url.as_str()).collect::<Vec<_>>(), ["http://ex.co"; 2]);
    let texts = [input[0].text.as_str(), input[1].text.as_str()];
    assert_eq!(reassemble(&texts, &segments), "http://ex.co");
}

#[test]
fn an_emoji_cluster_row_that_does_not_fill_the_grid_must_not_join() {
    let cluster = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}x.ts";
    let input = vec![
        RowLinkInput { text: cluster.to_string(), columns: 6, links: Vec::new() },
        RowLinkInput { text: ":9".to_string(), columns: 2, links: Vec::new() },
    ];
    assert_eq!(cluster.encode_utf16().count(), 15, "the count that would fabricate a join");
    assert_eq!(compute_row_links(&input, 8, Some(&stub_resolve), None), vec![]);
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
    let mut urls: Vec<&str> = segments.iter().map(|segment| segment.url.as_str()).collect();
    urls.dedup();
    assert_eq!(urls, ["https://a.com/foo", "https://b.com/bar"]);
}

#[test]
fn a_path_with_a_slash_and_line_becomes_an_internal_file_link() {
    let segments = detect_files(&["Edited apps/web/src/FolderList.tsx:142 ok"]);
    let file = segments.iter().find(|segment| segment.file.is_some()).unwrap();
    assert_eq!(file.url, "/file/W/apps/web/src/FolderList.tsx#L142");
    assert_eq!(file.file.as_ref().unwrap().hint, "Open apps/web/src/FolderList.tsx:142");
}

#[test]
fn an_absolute_path_resolves_and_a_version_string_does_not() {
    assert!(has_file(&["see /Users/you/x.rs here"]));
    assert!(!has_file(&["v1.2.3 shipped"]));
}

#[test]
fn windows_drive_unc_and_backslash_paths_reach_the_resolver_intact() {
    let seen = std::cell::RefCell::new(Vec::new());
    let resolve = |path: &str, line: Option<u64>, _: Option<&str>| {
        seen.borrow_mut().push((path.to_string(), line));
        Some("/file/windows".to_string())
    };
    let texts = [r"C:\Users\Ada\src\main.ts:42", r"\\server\share\logs\build.log:7", "D:/work/roost/readme.md"];
    compute_row_links(&rows(&texts), 120, Some(&resolve), None);
    assert_eq!(
        seen.into_inner(),
        vec![
            (r"C:\Users\Ada\src\main.ts".to_string(), Some(42)),
            (r"\\server\share\logs\build.log".to_string(), Some(7)),
            ("D:/work/roost/readme.md".to_string(), None),
        ]
    );
}

#[test]
fn a_bare_filename_links_only_with_a_line() {
    assert!(!has_file(&["see FolderList.tsx here"]));
    assert!(has_file(&["see FolderList.tsx:9 here"]));
}

#[test]
fn bare_archive_filenames_link_without_a_line() {
    assert!(has_file(&["see backup.zip here"]));
    assert!(has_file(&["archive.tar.gz ready"]));
    assert!(!has_file(&["see readme.txt here"]));
    assert!(!has_file(&["data.csv here"]));
}

#[test]
fn long_paths_in_command_block_output_link_correctly() {
    let row = "│ /Users/you/Code/project/docs/report_2026-07-13.zip                                 │";
    let files: Vec<_> = detect_files(&[row]).into_iter().filter(|segment| segment.file.is_some()).collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].url, "/file/W/Users/you/Code/project/docs/report_2026-07-13.zip");
}

#[test]
fn scheme_urls_are_not_evicted_by_overlapping_file_path_matches() {
    let segments = detect_files(&["see https://example.com/path/file.zip here"]);
    assert!(segments.iter().any(|segment| segment.file.is_none() && segment.url == "https://example.com/path/file.zip"));
    assert!(segments.iter().all(|segment| segment.file.is_none()));
}

#[test]
fn custom_schemes_cannot_be_reinterpreted_as_worker_file_paths() {
    for target in ["vscode://file/src/main.ts", "vscode:file/src/main.ts", "custom:folder/report.zip"] {
        assert_eq!(detect_files(&[&format!("open {target} now")]), vec![], "{target}");
    }
}

#[test]
fn home_prefixed_paths_are_matched_whole_not_partially_as_absolute() {
    let files: Vec<_> = detect_files(&["see ~/Code/proj/a.py here"]).into_iter().filter(|segment| segment.file.is_some()).collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].url.contains("~/Code/proj/a.py"), "{}", files[0].url);
}

#[test]
fn home_paths_are_not_linkified_when_the_resolver_rejects_them() {
    let real = |path: &str, _: Option<u64>, _: Option<&str>| {
        (!path.starts_with('~')).then(|| format!("/file/W/{}", path.strip_prefix('/').unwrap_or(path)))
    };
    let count = |text: &str| {
        compute_row_links(&rows(&[text]), 80, Some(&real), None).iter().filter(|segment| segment.file.is_some()).count()
    };
    assert_eq!(count("see ~/Code/proj/a.py here"), 0);
    assert_eq!(count("see /Users/you/a.py here"), 1);
}

#[test]
fn without_a_resolver_there_are_no_file_links() {
    assert_eq!(detect(&["apps/web/foo.ts:1"], 80), vec![]);
}

#[test]
fn localhost_port_becomes_an_http_url_and_bare_localhost_does_not() {
    let segments = detect(&["Local: localhost:5174/app now"], 80);
    assert!(segments.iter().any(|segment| segment.url == "http://localhost:5174/app"));
    assert_eq!(detect(&["run on localhost soon"], 80), vec![]);
}

#[test]
fn github_refs_resolve_self_contained_or_against_the_session_repo() {
    let urls = |text: &str, owner_repo: Option<&str>| -> Vec<String> {
        compute_row_links(&rows(&[text]), 80, None, owner_repo).into_iter().map(|segment| segment.url).collect()
    };
    assert_eq!(urls("fixes owner/repo#412 today", None), ["https://github.com/owner/repo/issues/412"]);
    assert_eq!(urls("at owner/repo@deadbeef now", None), ["https://github.com/owner/repo/commit/deadbeef"]);
    assert_eq!(urls("see #7 and a1b2c3d4 here", None), Vec::<String>::new());
    assert_eq!(
        urls("see #7 and a1b2c3d4 here", Some("o/r")),
        ["https://github.com/o/r/issues/7", "https://github.com/o/r/commit/a1b2c3d4"]
    );
    assert!(urls("build 1234567 done", Some("o/r")).iter().all(|url| !url.contains("/commit/")));
}

#[test]
fn a_painted_producer_uri_wins_its_span_over_the_identical_regex_url() {
    let text = "https://github.com/cefege/roost";
    let link = PaintedLink { start: 0, end: text.len(), uri: format!("{text}/?custom=1"), key: PAINTED_KEY.into() };
    assert_eq!(compute_row_links(&[narrow(text, vec![link])], 80, None, None), vec![]);
    assert_eq!(detect(&[text], 80), vec![url_segment(0, 0, text.len(), text)]);
}

#[test]
fn a_valid_painted_file_target_stays_authoritative_including_across_a_soft_wrap() {
    let text = "/Users/you/Code/project/docs/report_2026-07-13.zip";
    let at = text.find("report_2026-07-13.zip").unwrap();
    let link = PaintedLink { start: at, end: text.len(), uri: format!("file://{text}"), key: PAINTED_KEY.into() };
    assert_eq!(compute_row_links(&[narrow(text, vec![link])], 80, Some(&stub_resolve), None), vec![]);
    let half = |start, end| PaintedLink { start, end, uri: "file:///tmp/Foo.txt".into(), key: PAINTED_KEY.into() };
    let wrapped = vec![
        RowLinkInput { text: "see /tmp/F".into(), columns: 10, links: vec![half(9, 10)] },
        RowLinkInput { text: "oo.txt".into(), columns: 6, links: vec![half(0, 6)] },
    ];
    assert_eq!(compute_row_links(&wrapped, 10, Some(&stub_resolve), None), vec![]);
}

#[test]
fn the_prefilter_keeps_every_link_family_detectable() {
    let resolve = |path: &str, line: Option<u64>, _: Option<&str>| {
        Some(format!("/file/fp/{path}{}", line.map_or(String::new(), |line| format!("#L{line}"))))
    };
    let one = |text: &str, owner_repo: Option<&str>| -> Vec<String> {
        compute_row_links(&rows(&[text]), 80, Some(&resolve), owner_repo).into_iter().map(|segment| segment.url).collect()
    };
    assert_eq!(one("open https://example.com now", None), ["https://example.com"]);
    assert_eq!(one("mail me at mailto:a@b.co", None), Vec::<String>::new());
    assert_eq!(one("Local:   localhost:5174/", None), ["http://localhost:5174/"]);
    assert_eq!(one("at apps/web/src/foo.ts:42 exactly", None), ["/file/fp/apps/web/src/foo.ts#L42"]);
    assert_eq!(one("see foo.ts:9 there", None), ["/file/fp/foo.ts#L9"]);
    assert_eq!(one("grab release.tar.gz please", None), ["/file/fp/release.tar.gz"]);
    assert_eq!(one("fixed cefege/roost#12 today", None), ["https://github.com/cefege/roost/issues/12"]);
    assert_eq!(one("landed cefege/roost@deadbeef ok", None), ["https://github.com/cefege/roost/commit/deadbeef"]);
    assert_eq!(one("closes #77 finally", Some("cefege/roost")), ["https://github.com/cefege/roost/issues/77"]);
    assert_eq!(one("reverted deadbeef1 earlier", Some("cefege/roost")), ["https://github.com/cefege/roost/commit/deadbeef1"]);
    assert_eq!(one("the quick brown fox jumps over a lazy dog", Some("cefege/roost")), Vec::<String>::new());
}

#[test]
fn the_prefilter_skips_a_hint_free_line_and_leaves_its_painted_anchor_alone() {
    let link = PaintedLink { start: 5, end: 11, uri: "https://x.test/readme".into(), key: PAINTED_KEY.into() };
    assert_eq!(compute_row_links(&[narrow("open Readme now", vec![link])], 80, None, None), vec![]);
}

fn gesture(button: i16, ctrl: bool, meta: bool) -> LinkActivationGesture {
    LinkActivationGesture { button, ctrl, meta, shift: false, alt: false }
}

#[test]
fn physical_ctrl_and_meta_gestures_remain_platform_specific() {
    let (mac, other) = (LinkModifierKey::for_platform(true), LinkModifierKey::for_platform(false));
    let (meta, ctrl) = (gesture(0, false, true), gesture(0, true, false));
    assert!(is_link_activation_gesture(&meta, false, mac));
    assert!(!is_link_activation_gesture(&ctrl, false, mac));
    assert!(is_link_activation_gesture(&ctrl, false, other));
    assert!(!is_link_activation_gesture(&meta, false, other));
    // Compact arming opens with no modifier; a right or shifted click never does.
    assert!(is_link_activation_gesture(&gesture(0, false, false), true, mac));
    assert!(!is_link_activation_gesture(&gesture(2, false, false), true, mac));
    assert!(!is_link_activation_gesture(&LinkActivationGesture { shift: true, ..ctrl }, false, other));
}

#[test]
fn the_modifier_level_is_total_and_an_event_without_modifier_fields_is_not_held() {
    assert!(!is_link_modifier_held(&LinkActivationGesture::default(), LinkModifierKey::Meta));
    assert!(!is_link_modifier_held(&LinkActivationGesture::default(), LinkModifierKey::Control));
    assert!(is_link_modifier_held(&gesture(0, false, true), LinkModifierKey::Meta));
    assert!(!is_link_modifier_held(&gesture(0, true, false), LinkModifierKey::Meta));
}

#[test]
fn armed_and_physical_modifier_terminal_links_bypass_pty_bytes_while_bare_clicks_forward() {
    let mac = LinkModifierKey::Meta;
    let withheld = |over, modifier: &LinkActivationGesture, armed| withhold_press(over, modifier, armed, mac, false);
    assert_eq!(withheld(true, &gesture(0, false, true), false), Some(PressWithheld::LinkActivation));
    assert_eq!(withheld(true, &gesture(0, false, false), false), None);
    assert_eq!(withheld(true, &gesture(0, false, false), true), Some(PressWithheld::LinkActivation));
    assert_eq!(withheld(false, &gesture(0, false, false), true), None);
    let middle = withhold_press(true, &gesture(1, false, false), true, mac, true);
    assert_eq!(middle, Some(PressWithheld::DeckMiddleButton));
}

#[test]
fn edge_matches_agree_with_the_v2_regex_battery() {
    // Expected values are v2 `computeRowLinks` output for the same input: the
    // `(?<![,.])` backtrack, bracketed suffixes, IPv6 ports, directory
    // backtracking, archive `\b`, and Windows drive/UNC paths with columns.
    let spans = |text: &str, files: bool| -> Vec<(usize, usize, String)> {
        let resolve = files.then_some(&stub_resolve as &dyn Fn(&str, Option<u64>, Option<&str>) -> Option<String>);
        compute_row_links(&rows(&[text]), 80, resolve, None)
            .into_iter()
            .map(|segment| (segment.start, segment.end, segment.url))
            .collect()
    };
    let owned = |expected: &[(usize, usize, &str)]| -> Vec<(usize, usize, String)> {
        expected.iter().map(|(start, end, url)| (*start, *end, url.to_string())).collect()
    };
    assert_eq!(spans("x data://.. y http://a.(b). z", false), owned(&[(14, 26, "http://a.(b)")]));
    assert_eq!(
        spans("open src/foo.ts/bar and ../a/b.tar.lz4 and x.zip-y", true),
        owned(&[(5, 15, "/file/W/src/foo.ts"), (24, 38, "/file/W/../a/b.tar.lz4"), (43, 48, "/file/W/x.zip")])
    );
    assert_eq!(
        spans("http://[::1]:80., http://a,b,.; mailto:x@y.z", false),
        owned(&[(0, 15, "http://[::1]:80"), (18, 31, "http://a,b,.;")])
    );
    assert_eq!(
        spans(r"C:\a\b.ts:3:4 \\h\s\c.md:1", true),
        owned(&[(0, 13, r"/file/W/C:\a\b.ts#L3"), (14, 26, r"/file/W/\\h\s\c.md#L1")])
    );
}
