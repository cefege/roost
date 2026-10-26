# Vendored `quickjs.wasm` (quickjs-wasi 3.6.2)

Source artifact for the single JS engine of the codemode sandbox
(ADR-0034 decision 3, ADR-0035; replaces the ADR-0001 "no JS" red line only
for this QuickJS-via-WASM form).

- npm package: `quickjs-wasi@3.6.2`
- Upstream repository: `git://github.com/vercel-labs/quickjs-wasi.git`
- Tarball: `https://registry.npmjs.org/quickjs-wasi/-/quickjs-wasi-3.6.2.tgz`
- Tarball sha256: `f1f4349f19a2d849e33ea0ae9bec2e7062b8839f4eceb17c9051ddbaa2720982`
- npm dist.integrity: `sha512-FCqGtGOrMgzUiIrMNMA2YnsOxCNwo31dzqXvclXUC6xeT35NJLKXQJsvbeCTjvoFAwZgEAPg8U6+KAPDGXn8Mg==`
- npm dist.shasum: `6ab7ed689997a9013b49ea7bbce3e88896bd80d9`
- `quickjs.wasm` sha256: `d4c9375f2b1ca4dc95f72c8aa2982a7a9951ac8011490d79c6582df732b4bbd9`
- `quickjs.wasm` size: 637405 bytes
- Retrieved: 2026-10-02 (files copied out of the official tarball; the file
  hash was verified against the installed package copy as a second source)

No patches are applied: the binary is embedded byte-for-byte by
`crates/rpi-codemode/src/wasm.rs` (`include_bytes!`), and the sha256 above is
asserted by `wasm::verify_embedded_wasm()` in tests (G1/vendor gate). The
sandbox drives the module through the `qjs_*` C ABI exported by the binary
(the same ABI `quickjs-wasi`'s own JS wrapper consumes), not through any
Node/Bun glue.

`LICENSE` next to this file is the package's MIT license text.

## rpi host shims (`random_get`)

The worker supplies deterministic mode with a seeded Mulberry32 stream and a
virtual WASI clock; its UTC timezone shim and prelude lock down observable
date/random behavior for replay. The legacy non-deterministic execution path
still uses `RandomState` for `random_get`. Neither path treats this import as
a CSPRNG; untrusted scripts have no network or host-I/O capability.