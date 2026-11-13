//! Semantic link safety boundaries shared by anchors, pointer actions and menus.
//! Exercises external schemes, authenticated worker files and host requests.
//! Runs under the semantic links module without browser effects.

use super::*;

const WORKER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn http_and_https_links_stay_browser_owned() {
    assert_eq!(
        resolve_native_target("https://example.com/a?b=1", WORKER, "/work", Some("linux")),
        NativeTarget::External {
            href: "https://example.com/a?b=1".into(),
            scheme: "https"
        }
    );
    assert_eq!(
        resolve_native_target("mailto:ops@example.com", WORKER, "/work", Some("linux")),
        NativeTarget::External {
            href: "mailto:ops@example.com".into(),
            scheme: "mailto"
        }
    );
}

#[test]
fn unsafe_schemes_and_relative_text_stay_inert() {
    for raw in [
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:msgbox(1)",
        "https://",
        "JaVaScRiPt:alert(1)",
        " https://example.com",
        "https://example.com/\nnext",
        "mailto:ops@example.com\r",
        "mailto:",
        "file:relative",
        "just some text",
    ] {
        assert_eq!(
            resolve_native_target(raw, WORKER, "/work", Some("linux")),
            NativeTarget::Inert,
            "{raw}"
        );
    }
}

#[test]
fn raw_controls_in_file_routes_stay_inert() {
    for path in [
        "a b.txt",
        "a\nb.txt",
        "a\rb.txt",
        "a\tb.txt",
        "a\u{7f}b.txt",
    ] {
        let target = format!("/file/{WORKER}/{path}");
        assert_eq!(
            resolve_native_target(&target, WORKER, "/work", Some("linux")),
            NativeTarget::Inert,
            "{target:?}",
        );
    }
}

#[test]
fn worker_paths_use_the_authenticated_file_route_and_keep_the_line() {
    let NativeTarget::WorkerFile { href } = resolve_native_target(
        "/srv/roost/README.md:12",
        WORKER,
        "/srv/roost",
        Some("linux"),
    ) else {
        panic!("a worker path resolves");
    };
    assert!(href.starts_with("/file/"));
    assert!(href.ends_with("#L12"), "{href}");

    let NativeTarget::WorkerFile { href } = resolve_native_target(
        "file:///srv/roost/a%20b.txt",
        WORKER,
        "/srv/roost",
        Some("linux"),
    ) else {
        panic!("a file URI resolves");
    };
    assert!(
        href.contains("a%20b.txt") || href.contains("a b.txt"),
        "{href}"
    );
}

#[test]
fn an_already_minted_file_route_is_not_resolved_again_against_the_cwd() {
    let route = format!("/file/{WORKER}/srv%2Froost%2FREADME.md#L4");
    assert_eq!(
        resolve_native_target(&route, WORKER, "/srv/roost", Some("linux")),
        NativeTarget::WorkerFile { href: route }
    );
}

#[test]
fn oversized_urls_and_file_routes_never_reach_browser_navigation() {
    let suffix = "x".repeat(roost_protocol::cell::MAX_LINK_URI_BYTES);
    for target in [
        format!("https://example.com/{suffix}"),
        format!("mailto:{suffix}@example.com"),
        format!("/file/{WORKER}/{suffix}"),
    ] {
        assert_eq!(
            resolve_native_target(&target, WORKER, "/work", Some("linux")),
            NativeTarget::Inert,
        );
    }
}

#[test]
fn file_authorities_stay_on_the_owning_worker_not_the_browsers_disk() {
    for (target, cwd, os) in [
        ("file://fileserver/srv/report.txt", "/work", "linux"),
        ("file://fileserver/share/report.txt", "C:\\work", "win32"),
    ] {
        let resolved = resolve_native_target(target, WORKER, cwd, Some(os));
        let NativeTarget::WorkerFile { href } = &resolved else {
            panic!("{target} must resolve through its authenticated viewer, got {resolved:?}");
        };
        assert!(href.starts_with(&format!("/file/{WORKER}/")), "{href}");
        assert!(!href.starts_with("file:"), "{href}");
    }
}

#[test]
fn host_open_refuses_non_http_external_schemes() {
    let refusal = resolve_host_open("mailto:ops@example.com", WORKER, "/work", Some("linux"))
        .expect_err("mailto is not a host service target");
    assert!(refusal.contains("mailto"));
    assert_eq!(
        resolve_host_open("https://example.com/cb", WORKER, "/work", Some("linux")),
        Ok(HostOpenTarget::Browser("https://example.com/cb".into()))
    );
    assert!(matches!(
        resolve_host_open("/work/file.txt", WORKER, "/work", Some("linux")),
        Ok(HostOpenTarget::WorkerFile(_))
    ));
    for callback in [
        "http://127.0.0.1:8976/callback",
        "http://localhost:8976/callback",
        "https://[::1]:8976/callback",
    ] {
        let refusal = resolve_host_open(callback, WORKER, "/work", Some("linux"))
            .expect_err("a worker-local callback is not this browser's localhost");
        assert!(
            refusal.contains("localhost") || refusal.contains("127.0.0.1"),
            "{refusal}"
        );
    }
}

#[test]
fn only_a_minted_dashboard_capability_path_opens() {
    let token = "0123456789abcdef".repeat(4);
    let path = capability_path(&token);
    assert_eq!(resolve_stats_capability(&path), Ok(path.clone()));
    for refused in [
        "/omp-stats/".to_owned() + &token,
        format!("{path}api/stats"),
        format!("/omp-stats/{token}?x=1"),
        "http://127.0.0.1:3847/".to_owned(),
        format!("/omp-stats/{token}/../"),
    ] {
        assert!(resolve_stats_capability(&refused).is_err(), "{refused}");
    }
}

#[test]
fn missing_owner_context_preserves_external_links_but_never_mints_worker_routes() {
    assert_eq!(
        resolve_native_target_in_context("https://example.com/evidence", None),
        NativeTarget::External {
            href: "https://example.com/evidence".to_owned(),
            scheme: "https"
        },
    );
    assert_eq!(
        resolve_native_target_in_context("mailto:ops@example.com", None),
        NativeTarget::External {
            href: "mailto:ops@example.com".to_owned(),
            scheme: "mailto"
        },
    );
    for target in [
        "/srv/work/report.txt",
        "file:///srv/work/report.txt",
        "/file/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/report.txt",
        "javascript:alert(1)",
    ] {
        assert_eq!(
            resolve_native_target_in_context(target, None),
            NativeTarget::Inert,
            "{target}"
        );
    }
}
