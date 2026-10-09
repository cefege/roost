# `third_party/`

Vendored crates, and nothing else. A crate lands here only when the
upstream public API cannot express something Roost requires and the gap is a
real, demonstrated one — not because a patch would be convenient.

Rules for anything in this directory:

1. **Vendor the exact crates.io version**, unmodified except for the smallest
   change that closes the gap. Record the version in the crate's
   `Cargo.toml`.
2. **Wire it with `[patch.crates-io]`** in the root `Cargo.toml`, pointing at
   the local path. A vendored copy that is not actually being compiled is
   worse than no copy.
3. **List every change in `ROOST-PATCHES.md`** beside the crate: what the
   upstream API could not do, the hunk, and the invariant the hunk protects.
   A vendored patch with no written reason is an unreviewable dependency.
4. **Keep the upstream test suite green.** If a patched crate's own tests
   fail, the patch broke something nobody intended to break.
5. **Add a conformance vector for the behavior the patch enables.** The
   vector is language-neutral and lives under the `protocol/conformance/`
   family that covers it, so the patched behavior is pinned against something
   other than this repository's own implementation.

## Contents

- **`rio_vt/`** — crates.io `rio-vt` 0.5.28 (MIT), Rio's terminal core,
  which decodes the kitty graphics protocol, sixel and iTerm2 inline images.
  Wired through `[patch.crates-io]`; outside the workspace, so its suite runs
  by manifest path:
  `cargo test --manifest-path third_party/rio_vt/Cargo.toml --no-default-features --features graphics`.
  [`rio_vt/ROOST-PATCHES.md`](rio_vt/ROOST-PATCHES.md) lists patches R1–R7:
  LF clearing a pending wrap, delete discarding rather than reaching history,
  ED clearing the viewport in place, relative cursor motion bounded by DECSTBM
  margins, a per-row Roost prompt mark, and a listener hook for dropped CSI
  sequences with program-input noise logged at debug, and `simdutf` made optional
  so no C++ is compiled.
