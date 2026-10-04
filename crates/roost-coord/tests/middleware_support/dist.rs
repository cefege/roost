// The web build the middleware fixture serves: a shell, one hashed bundle, a
// stable-named icon, and the wasm a dx build ships with its `.br` and `.gz`
// siblings. Owned by `middleware_support`, which mounts it when a test asks
// for `serve_dist`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// The wasm the fixture build ships with both precompressed siblings.
pub const WASM_PATH: &str = "/assets/app_bg.a1b2c3.wasm";
/// A wasm the fixture build ships with no sibling at all.
pub const BARE_WASM_PATH: &str = "/assets/bare_bg.d4e5f6.wasm";
/// The raw bytes of both fixture wasm files.
pub const WASM_RAW: &[u8] = b"\0asm\x01\0\0\0 the raw module";
/// The `.br` sibling's bytes: a marker, so a test can prove they went out
/// verbatim rather than through a compressor.
pub const WASM_BROTLI: &[u8] = b"brotli sibling bytes";
/// The `.gz` sibling's bytes, a marker for the same reason.
pub const WASM_GZIP: &[u8] = b"\x1f\x8b gzip sibling bytes";
const WASM_BROTLI_SIBLING: &str = "assets/app_bg.a1b2c3.wasm.br";
const WASM_GZIP_SIBLING: &str = "assets/app_bg.a1b2c3.wasm.gz";

/// A minimal but complete web build under `root`: the shell, one hashed
/// bundle, and a stable-named icon, because those three are the three answers
/// the front door has to keep distinct; plus a wasm with the `.br` and `.gz`
/// siblings a dx build writes, and one wasm without any.
pub fn write_dist(root: &Path) -> PathBuf {
    let dist = root.join("dist");
    std::fs::create_dir_all(dist.join("assets")).expect("the build's directory");
    std::fs::write(
        dist.join("index.html"),
        b"<!doctype html><title>roost</title>",
    )
    .expect("the build's shell");
    std::fs::write(
        dist.join("assets/app.a1b2c3.js"),
        b"export const shell = 1;",
    )
    .expect("the build's bundle");
    std::fs::write(dist.join("favicon.ico"), b"icon").expect("the build's icon");
    std::fs::write(dist.join(WASM_PATH.trim_start_matches('/')), WASM_RAW)
        .expect("the build's wasm");
    std::fs::write(dist.join(WASM_BROTLI_SIBLING), WASM_BROTLI).expect("the wasm's .br");
    std::fs::write(dist.join(WASM_GZIP_SIBLING), WASM_GZIP).expect("the wasm's .gz");
    std::fs::write(dist.join(BARE_WASM_PATH.trim_start_matches('/')), WASM_RAW)
        .expect("a wasm with no siblings");
    dist
}
