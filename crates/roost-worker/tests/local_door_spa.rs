//! The door's pages (v2 `local-ui-server.test.ts` "unknown paths reach the SPA
//! responder; writes do not", over the real responder of
//! `packages/host/src/spa.ts`): a deep link is the shell, a missing bundle is
//! a 404 rather than HTML, and a door with no build says so on every page.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod door_support;
#[path = "credential_support/scratch.rs"]
mod scratch;

use door_support::{DoorOptions, request, start_door};
use scratch::Scratch;
use tokio::io::AsyncReadExt as _;

const SHELL: &str = "<!doctype html><title>roost</title>";
const BUNDLE: &str = "console.log('roost bundle');";

fn build() -> Scratch {
    let dist = Scratch::new("door-spa");
    std::fs::create_dir_all(dist.path("assets")).unwrap();
    std::fs::write(dist.path("index.html"), SHELL).unwrap();
    std::fs::write(dist.path("assets/app-1234.js"), BUNDLE).unwrap();
    dist
}

async fn gunzip(bytes: &[u8]) -> String {
    let mut decoder = async_compression::tokio::bufread::GzipDecoder::new(bytes);
    let mut text = String::new();
    decoder
        .read_to_string(&mut text)
        .await
        .expect("a gzip body");
    text
}

/// A deep link is not a file and not under `assets/`, so it is the shell,
/// never cached; a write to the same path is refused rather than paged.
#[tokio::test]
async fn a_deep_link_is_the_uncached_shell_and_a_write_is_refused() {
    let dist = build();
    let door = start_door(DoorOptions {
        web_dist: Some(dist.root()),
        ..DoorOptions::default()
    })
    .await;

    let page = request(&door, "GET", "/w/some-workspace", &[]).await;
    let zipped = request(
        &door,
        "GET",
        "/w/some-workspace/t/7",
        &[("accept-encoding", "gzip")],
    )
    .await;
    let posted = request(&door, "POST", "/w/some-workspace", &[]).await;

    assert_eq!(page.status, 200);
    assert_eq!(std::str::from_utf8(&page.body).unwrap(), SHELL);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        page.header("cache-control"),
        Some("no-cache, no-store, must-revalidate")
    );
    assert_eq!(zipped.header("content-encoding"), Some("gzip"));
    assert_eq!(gunzip(&zipped.body).await, SHELL);
    assert_eq!(posted.status, 405);
    assert!(posted.body.is_empty());
}

/// A content-hashed bundle is immutable and a stale reference to one is a
/// 404: answering it with HTML would hand a page to a script tag.
#[tokio::test]
async fn a_bundle_is_served_immutable_and_a_missing_one_is_not_the_shell() {
    let dist = build();
    let door = start_door(DoorOptions {
        web_dist: Some(dist.root()),
        ..DoorOptions::default()
    })
    .await;

    let bundle = request(&door, "GET", "/assets/app-1234.js", &[]).await;
    let stale = request(&door, "GET", "/assets/app-0000.js", &[]).await;

    assert_eq!(bundle.status, 200);
    assert_eq!(std::str::from_utf8(&bundle.body).unwrap(), BUNDLE);
    assert_eq!(
        bundle.header("content-type"),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        bundle.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(bundle.header("vary"), Some("accept-encoding"));
    assert_eq!(stale.status, 404);
}

/// The wasm goes out as dx's `.br` sibling first and its `.gz` sibling when
/// brotli is refused, each verbatim: compressing a sibling again would hand the
/// browser the sibling instead of the module.
#[tokio::test]
async fn the_wasm_goes_out_as_its_precompressed_sibling_verbatim() {
    let dist = build();
    std::fs::write(dist.path("assets/app_bg-1234.wasm"), b"\0asm raw module").unwrap();
    std::fs::write(dist.path("assets/app_bg-1234.wasm.br"), b"brotli sibling").unwrap();
    std::fs::write(
        dist.path("assets/app_bg-1234.wasm.gz"),
        b"\x1f\x8b gzip sibling",
    )
    .unwrap();
    let door = start_door(DoorOptions {
        web_dist: Some(dist.root()),
        ..DoorOptions::default()
    })
    .await;

    let brotli = request(
        &door,
        "GET",
        "/assets/app_bg-1234.wasm",
        &[("accept-encoding", "gzip, br")],
    )
    .await;
    let gzip = request(
        &door,
        "GET",
        "/assets/app_bg-1234.wasm",
        &[("accept-encoding", "gzip")],
    )
    .await;

    assert_eq!(brotli.header("content-encoding"), Some("br"));
    assert_eq!(brotli.header("content-type"), Some("application/wasm"));
    assert_eq!(brotli.body, b"brotli sibling");
    assert_eq!(gzip.header("content-encoding"), Some("gzip"));
    assert_eq!(gzip.body, b"\x1f\x8b gzip sibling");
    assert_eq!(gzip.header("vary"), Some("accept-encoding"));
}

/// With no build the door still answers its bootstrap, and every page is a
/// 404 rather than an empty page that looks like a broken bundle.
#[tokio::test]
async fn a_door_with_no_build_answers_404_for_every_page() {
    let door = start_door(DoorOptions::default()).await;

    let root = request(&door, "GET", "/", &[]).await;
    let deep = request(&door, "GET", "/w/some-workspace", &[]).await;

    assert_eq!(root.status, 404);
    assert_eq!(deep.status, 404);
}
