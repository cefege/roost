//! Untrusted terminal-authored link targets, classified before any anchor is
//! painted.
//!
//! Terminal output is hostile input, so every case here is a refusal: a
//! javascript: URL, a protocol-relative `//host/share`, a `file://` target
//! with credentials, a link over the wire cap. A painted anchor is a link
//! because the core said so, at the cells the core said — never because
//! something in the text looked like a URL.

use roost_web_terminal::{TerminalLinkTarget, classify_terminal_link_target};

fn external(raw: &str) -> TerminalLinkTarget {
    TerminalLinkTarget::External {
        href: raw.to_string(),
        display: raw.to_string(),
    }
}

fn file(raw_path: &str, line: Option<u32>, display: &str) -> TerminalLinkTarget {
    TerminalLinkTarget::WorkerFile {
        raw_path: raw_path.to_string(),
        line,
        file_authority: None,
        display: display.to_string(),
    }
}

#[test]
fn an_absolute_http_url_is_the_only_target_that_carries_an_href() {
    for raw in [
        "https://example.com/a?b=1#c",
        "http://localhost:5173/",
        "https://[::1]:4102/x",
        "HTTPS://EXAMPLE.COM",
    ] {
        assert_eq!(
            classify_terminal_link_target(raw),
            Some(external(raw)),
            "{raw}"
        );
    }
}

#[test]
fn an_http_target_without_a_reachable_host_is_refused() {
    for raw in [
        "https://",
        "http:///path",
        "http://ho st/",
        "http://[::1/x",
        "http://ex^ample.com",
    ] {
        assert_eq!(classify_terminal_link_target(raw), None, "{raw}");
    }
}

#[test]
fn a_scheme_that_is_neither_http_nor_file_never_becomes_an_anchor() {
    for raw in [
        "javascript:alert(1)",
        "vscode:file/a.ts",
        "data:text/html,x",
        "mailto:a@b.dev",
        "vbscript:msgbox",
    ] {
        assert_eq!(classify_terminal_link_target(raw), None, "{raw}");
    }
}

#[test]
fn a_protocol_relative_target_is_ambiguous_until_a_worker_resolver_accepts_it() {
    assert_eq!(classify_terminal_link_target("//server/share/a.ts"), None);
}

#[test]
fn a_file_uri_keeps_its_path_and_never_gains_an_href() {
    assert_eq!(
        classify_terminal_link_target("file:///home/dev/a.ts"),
        Some(file("/home/dev/a.ts", None, "file:///home/dev/a.ts"))
    );
    assert_eq!(
        classify_terminal_link_target("file:///C:/src/a.ts:12"),
        Some(file("C:/src/a.ts", Some(12), "file:///C:/src/a.ts:12"))
    );
    assert_eq!(
        classify_terminal_link_target("file:///a/b.ts#L9"),
        Some(file("/a/b.ts", Some(9), "file:///a/b.ts#L9"))
    );
    assert_eq!(
        classify_terminal_link_target("file:///a%20b/c.ts"),
        Some(file("/a b/c.ts", None, "file:///a%20b/c.ts"))
    );
}

#[test]
fn a_file_uri_with_credentials_a_query_or_a_bad_fragment_is_refused() {
    for raw in [
        "file://user:pw@host/a.ts",
        "file:///a.ts?x=1",
        "file:///a.ts#L0",
        "file:///a.ts#Lx",
        "file:///a.ts#9",
        "file:relative/a.ts",
        "file:///a%2.ts",
    ] {
        assert_eq!(classify_terminal_link_target(raw), None, "{raw}");
    }
}

#[test]
fn a_printed_path_is_a_file_target_with_its_line_split_off() {
    assert_eq!(
        classify_terminal_link_target("/srv/app/main.rs:4"),
        Some(file("/srv/app/main.rs", Some(4), "/srv/app/main.rs:4"))
    );
    assert_eq!(
        classify_terminal_link_target("C:\\src\\a.ts"),
        Some(file("C:\\src\\a.ts", None, "C:\\src\\a.ts"))
    );
    assert_eq!(
        classify_terminal_link_target("./relative/b.py"),
        Some(file("./relative/b.py", None, "./relative/b.py"))
    );
    assert_eq!(
        classify_terminal_link_target("a.ts:9:3"),
        Some(file("a.ts", Some(9), "a.ts:9:3")),
        "a column is consumed: the viewer has a line contract and no column contract"
    );
}

#[test]
fn prose_is_not_a_path_and_is_not_linkified() {
    for raw in ["hello", "version 1.2.3 released", "...", "/", "e.g. something"] {
        assert_eq!(classify_terminal_link_target(raw), None, "{raw}");
    }
}

#[test]
fn a_bare_file_name_with_a_valid_extension_is_a_target() {
    assert_eq!(
        classify_terminal_link_target("notes.md"),
        Some(file("notes.md", None, "notes.md"))
    );
    assert_eq!(
        classify_terminal_link_target("archive.tar.gz"),
        Some(file("archive.tar.gz", None, "archive.tar.gz"))
    );
    assert_eq!(
        classify_terminal_link_target("main.rs:12"),
        Some(file("main.rs", Some(12), "main.rs:12"))
    );
}

#[test]
fn a_target_that_cannot_be_linkified_anywhere_is_refused() {
    assert_eq!(classify_terminal_link_target(""), None);
    assert_eq!(classify_terminal_link_target(" https://x.dev"), None);
    assert_eq!(classify_terminal_link_target("https://x.dev "), None);
    assert_eq!(classify_terminal_link_target("https://x.dev/a\u{7}b"), None);
    assert_eq!(classify_terminal_link_target("notes\u{0}.md"), None);
    assert_eq!(
        classify_terminal_link_target(&format!("https://x.dev/{}", "a".repeat(2100))),
        None,
        "an over-cap URI loses the link and keeps the text"
    );
}
