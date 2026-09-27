//! The four decisions a static front door gets wrong, each as a named rule.
//!
//! Ported from the resolver tests `packages/host/src/spa.test.ts` pins, and
//! written against a real directory because every one of these is a filesystem
//! question: an in-memory map would let a traversal pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_host::spa_path::{
    ContentEncoding, SpaTarget, accepts_gzip, cache_control_for, content_type_for,
    is_compressible, resolve, resolve_disk_spa_root,
};

/// A fresh tree with one build in it. The build sits in a `dist/` subdirectory
/// so a traversal test has a real file to reach for that is still outside the
/// build, without either of them touching the shared temp directory.
fn build_root() -> PathBuf {
    let dist = fixture_tree("built").join("dist");
    fs::create_dir_all(dist.join("assets")).expect("the fixture creates its tree");
    fs::create_dir_all(dist.join("fonts")).expect("the fixture creates its tree");
    fs::write(dist.join("index.html"), b"<!doctype html>").expect("the fixture writes a shell");
    fs::write(dist.join("assets/app.a1b2c3.js"), b"export {};").expect("a hashed bundle");
    fs::write(dist.join("fonts/mono.woff2"), b"woff2").expect("a stable-named face");
    dist
}

/// A directory no other test in this binary can collide with. The counter is
/// what makes it so: these tests run in parallel threads inside one process,
/// and a tree named after the test alone would be three tests sharing a path.
fn fixture_tree(name: &str) -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let ordinal = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tree = std::env::temp_dir().join(format!(
        "roost-spa-path-{}-{name}-{ordinal}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir_all(&tree).expect("the fixture creates its tree");
    tree
}

fn remove(root: &Path) {
    if let Some(tree) = root.parent() {
        let _ = fs::remove_dir_all(tree);
    }
}

#[test]
fn a_deep_link_that_is_not_a_file_gets_the_shell() {
    let root = build_root();
    for path in [
        "/s/01HQ8Z",
        "/t/ab12cd34/",
        "/browse/ab12cd34",
        "/",
        "/w/old/t/chan",
    ] {
        assert_eq!(
            resolve(&root, path, ""),
            SpaTarget::IndexFallback {
                file: root.join("index.html")
            },
            "{path} is a client route, not a file, and must not 404"
        );
    }
    remove(&root);
}

#[test]
fn a_missing_content_hashed_bundle_is_never_the_shell() {
    let root = build_root();
    // The bundle name changed between two deploys. Falling back to HTML here
    // hands the browser a page where it expects JavaScript, and the failure
    // surfaces as an unparseable module rather than as a missing asset.
    assert_eq!(resolve(&root, "/assets/gone.deadbe.js", ""), SpaTarget::NotFound);
    assert_eq!(resolve(&root, "/assets/nested/deep/x.js", ""), SpaTarget::NotFound);
    remove(&root);
}

#[test]
fn a_traversal_cannot_reach_a_file_outside_the_build() {
    let root = build_root();
    let outside = root.parent().expect("the build is inside the fixture tree").join("secret.txt");
    fs::write(&outside, b"secret").expect("a neighbouring file");

    for path in [
        "/../secret.txt",
        "/assets/../../secret.txt",
        "/a/../../secret.txt",
    ] {
        assert_eq!(
            resolve(&root, path, ""),
            SpaTarget::NotFound,
            "{path} must be refused before the filesystem sees it"
        );
    }
    // A percent-encoded traversal is never decoded, so `%2e%2e` is a FILENAME
    // and not a traversal: it resolves to nothing, which makes it a deep link
    // like any other. The property that matters is that no answer names the
    // file outside the build.
    assert_eq!(
        resolve(&root, "/%2e%2e/secret.txt", ""),
        SpaTarget::IndexFallback {
            file: root.join("index.html")
        },
        "an encoded traversal must not become a read of the file it names"
    );
    remove(&root);
}

#[test]
fn a_root_with_no_shell_answers_not_found_for_everything() {
    let root = fixture_tree("empty").join("dist");
    fs::create_dir_all(root.join("assets")).expect("the fixture creates its tree");
    fs::write(root.join("assets/app.js"), b"export {};").expect("a bundle with no shell");

    // A build whose index.html is missing is a broken install, and the report
    // belongs in the boot log. A 404 per page is the same fact, 200 times.
    assert_eq!(resolve(&root, "/s/01HQ8Z", ""), SpaTarget::NotFound);
    assert_eq!(resolve_disk_spa_root(Some(&root)), None);
    remove(&root);
}

#[test]
fn a_precompressed_sibling_is_served_when_the_client_admits_it() {
    let root = build_root();
    fs::write(root.join("assets/app.a1b2c3.js.gz"), b"\x1f\x8b").expect("a sibling");

    assert_eq!(
        resolve(&root, "/assets/app.a1b2c3.js", "gzip, deflate, br"),
        SpaTarget::Asset {
            file: root.join("assets/app.a1b2c3.js.gz"),
            encoding: ContentEncoding::Gzip,
        }
    );
    // Without a sibling the same request is the raw file, and a caller that can
    // compress composes that from `is_compressible` and `accepts_gzip`.
    assert_eq!(
        resolve(&root, "/fonts/mono.woff2", "gzip"),
        SpaTarget::Asset {
            file: root.join("fonts/mono.woff2"),
            encoding: ContentEncoding::Identity,
        }
    );
    remove(&root);
}

#[test]
fn a_client_that_refuses_gzip_never_gets_a_compressed_body() {
    let root = build_root();
    fs::write(root.join("assets/app.a1b2c3.js.gz"), b"\x1f\x8b").expect("a sibling");

    for header in ["gzip;q=0", "identity", "br", "", "deflate, gzip;q=0.0"] {
        assert_eq!(
            resolve(&root, "/assets/app.a1b2c3.js", header),
            SpaTarget::Asset {
                file: root.join("assets/app.a1b2c3.js"),
                encoding: ContentEncoding::Identity,
            },
            "accept-encoding {header:?} does not admit a gzip body"
        );
    }
    assert!(accepts_gzip("gzip"));
    assert!(accepts_gzip("*"));
    assert!(!accepts_gzip("*;q=0"));
    remove(&root);
}

#[test]
fn a_name_the_bundle_reuses_never_becomes_immutable() {
    // v2's four cache cases, each with the deployment it protects.
    assert_eq!(
        cache_control_for("index.html"),
        "no-cache, no-store, must-revalidate"
    );
    assert_eq!(
        cache_control_for("assets/app.a1b2c3.js"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(cache_control_for("fonts/mono.woff2"), "public, max-age=604800");
    // A favicon keeps its name across builds, so caching it for a year pins
    // yesterday's icon in every browser that has ever visited.
    assert_eq!(cache_control_for("favicon.ico"), "no-cache");
    assert!(!is_compressible(Path::new("favicon.ico")));
    assert!(is_compressible(Path::new("app.js")));
    assert_eq!(content_type_for(Path::new("app.wasm")), "application/wasm");
    assert_eq!(
        content_type_for(Path::new("unknown.zzz")),
        "application/octet-stream"
    );
}
