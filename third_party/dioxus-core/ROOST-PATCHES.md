# Roost patches — dioxus-core 0.8.0-alpha.3

Exact crates.io source, MIT OR Apache-2.0. Wired through the workspace's
`[patch.crates-io]` entry; not a workspace member.

## Committed edge scans survive fragment and component recursion

`src/diff/node.rs`: carry `EdgeScan` through root traversal instead of resetting
it to live scope output. Placement scans also read each fragment child's
committed mounted vnode, not the parent fragment's cached child vnode.

A component can already have new render output while a sibling is mid-diff.
Mixing that new shape with old mounted slots (or a cached fragment shape with
updated slots) indexes a zero-length mount during ordinary loading-to-content
transitions. The public component API cannot repair the renderer's mount walk.

Guard: `file_viewer::loading_tests::loaded_preview_paints_file_lines_when_download_controls_appear`
in `roost-web`, driven by
`protocol/conformance/dom-placement/file-preview-loading.json`, plus the
upstream renderer suite. Browser smoke: open the real `/export` output in the
file viewer on desktop and phone.

## Standalone upstream test dependencies

The crates.io package omits its dev-dependencies. Restore the list from
`packages/core/Cargo.toml` at SDK commit
`cb27832398e8a3fa30a0fc8d6917c6d8a86fa71a`, resolving workspace versions
explicitly. `oracle/` is the unpublished `packages/oracle` at that exact
commit: Rust source is unchanged; its manifest only resolves workspace
dependencies locally. The standalone patch entry keeps its umbrella SDK
dependencies on the same core under test.

Run `cargo nextest run --manifest-path third_party/dioxus-core/Cargo.toml`
under the repository's machine-wide Cargo lock.
