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

Currently empty. The first entry is expected to be `alacritty_terminal`, if
the terminal-core conformance vectors prove that its public API cannot report
how many scrollback lines history rotation has discarded.
