//! File-path links: slash and line paths, absolute, Windows drive and UNC
//! paths, bare `name.ext:line` and archive names reach the worker resolver
//! intact, and a scheme or a rejected home path never becomes a worker file.
//! Test names follow v2's `apps/web/tests/renderer/terminal-links.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_links_detect_support;

use roost_web_terminal::links::compute_row_links;
use terminal_links_detect_support::{detect, detect_files, has_file, rows};

#[test]
fn a_path_with_a_slash_and_line_becomes_an_internal_file_link() {
    let segments = detect_files(&["Edited apps/web/src/FolderList.tsx:142 ok"]);
    let file = segments
        .iter()
        .find(|segment| segment.file.is_some())
        .unwrap();
    assert_eq!(file.url, "/file/W/apps/web/src/FolderList.tsx#L142");
    assert_eq!(
        file.file.as_ref().unwrap().hint,
        "Open apps/web/src/FolderList.tsx:142"
    );
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
    let texts = [
        r"C:\Users\Ada\src\main.ts:42",
        r"\\server\share\logs\build.log:7",
        "D:/work/roost/readme.md",
    ];
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
    let row =
        "│ /Users/you/Code/project/docs/report_2026-07-13.zip                                 │";
    let files: Vec<_> = detect_files(&[row])
        .into_iter()
        .filter(|segment| segment.file.is_some())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].url,
        "/file/W/Users/you/Code/project/docs/report_2026-07-13.zip"
    );
}

#[test]
fn scheme_urls_are_not_evicted_by_overlapping_file_path_matches() {
    let segments = detect_files(&["see https://example.com/path/file.zip here"]);
    assert!(segments.iter().any(
        |segment| segment.file.is_none() && segment.url == "https://example.com/path/file.zip"
    ));
    assert!(segments.iter().all(|segment| segment.file.is_none()));
}

#[test]
fn custom_schemes_cannot_be_reinterpreted_as_worker_file_paths() {
    for target in [
        "vscode://file/src/main.ts",
        "vscode:file/src/main.ts",
        "custom:folder/report.zip",
    ] {
        assert_eq!(
            detect_files(&[&format!("open {target} now")]),
            vec![],
            "{target}"
        );
    }
}

#[test]
fn home_prefixed_paths_are_matched_whole_not_partially_as_absolute() {
    let files: Vec<_> = detect_files(&["see ~/Code/proj/a.py here"])
        .into_iter()
        .filter(|segment| segment.file.is_some())
        .collect();
    assert_eq!(files.len(), 1);
    assert!(
        files[0].url.contains("~/Code/proj/a.py"),
        "{}",
        files[0].url
    );
}

#[test]
fn home_paths_are_not_linkified_when_the_resolver_rejects_them() {
    let real = |path: &str, _: Option<u64>, _: Option<&str>| {
        (!path.starts_with('~'))
            .then(|| format!("/file/W/{}", path.strip_prefix('/').unwrap_or(path)))
    };
    let count = |text: &str| {
        compute_row_links(&rows(&[text]), 80, Some(&real), None)
            .iter()
            .filter(|segment| segment.file.is_some())
            .count()
    };
    assert_eq!(count("see ~/Code/proj/a.py here"), 0);
    assert_eq!(count("see /Users/you/a.py here"), 1);
}

#[test]
fn without_a_resolver_there_are_no_file_links() {
    assert_eq!(detect(&["apps/web/foo.ts:1"], 80), vec![]);
}
