# Dependency and Rust/UI audit — 2026-11-16

Scope: workspace manifests, resolved registry dependencies, vendored terminal and
Dioxus patches, Rust/UI recipes, and build/release tooling. Versions below are
resolved versions, not merely the manifests' minimum requirements. This is a
dated audit, not a second dependency manifest.

## Updated dependencies

| Dependency | Previous | Updated | Practical benefit / migration |
| --- | --- | --- | --- |
| Rust toolchain and container builder | 1.98.1 | 1.100.0 | Aligned native/wasm/container builds; workspace MSRV 1.100. New Cargo unused-dependency diagnostics exposed an unused protocol dependency. Deprecated atomic `fetch_update` callers use `try_update`. |
| rio-vt and rio-graphics | 0.5.28 | 0.5.32 | Published terminal/parser/graphics fixes; local behavior retained as R1–R9. Synchronized input is parsed immediately while frame publication remains held. |
| Tokio | 1.53.1 | 1.54.1 | Runtime/I/O fixes, including the advisory fixed after 1.53.1. Cooperative `copy_buf` and file timestamp APIs are available; no speculative application changes. |
| tokio-util | 0.7.19 | 0.7.20 | Compatible utility fixes. |
| str0m | 0.23.1 | 0.25.0 | SDP/STUN panic and ICE fixes; newer DTLS/SCTP dependencies. Roost retains the supported default AWS-LC provider. |
| sysinfo | 0.37.2 | 0.40.4 | Windows ARM64 CPU-brand fix; fallible constructors and platform mappings adapted without panicking. |
| rsa, direct coordinator dependency | 0.9.10 | 0.10.0 | Fixed current RSA implementation; adapted public key components/signature verification. Private-key entropy feature is test-only. An older transitive RSA remains in Web Push. |
| protox | 0.9.1 | 0.10.0 | Bounded protobuf parser nesting; build-time only. Generated wire/JSON-name behavior retained. |
| prost-types, build-time only | 0.14.4 | 0.14.6 | Compatible descriptor tooling update; Buffa remains the only shipped protobuf runtime. |
| wasm-bindgen family | 0.2.127 | 0.2.130 | Coordinated SDK/CLI ABI update. |
| wasm-bindgen-futures | 0.4.77 | 0.4.80 | Kept aligned with wasm-bindgen. |
| js-sys and web-sys | 0.3.104 | 0.3.107 | Kept aligned with wasm-bindgen. |
| async-compression | 0.4.48 | 0.4.52 | Compatible compression fixes; explicit gzip/Tokio features preserved. |
| OpenSSL | 0.10.81 | 0.10.82 | Compatible crypto update, still vendored for fleet portability. |
| webpki-roots | 1.0.9 | 1.0.12 | Updated trust roots and advisory fixes. |
| libc | 0.2.189 | 0.2.191 | Platform binding fixes. |
| serde_json | 1.0.151 | 1.0.154 | Compatible JSON fixes. |
| sha2 | 0.11.0 | 0.11.1 | Compatible digest fixes. |
| reflink-copy | 0.1.30 | 0.1.33 | Compatible filesystem fixes. |

The resolver also refreshed compatible transitive packages, including ctutils
0.4.8, rustls-webpki 0.103.17, rustix 1.1.6, quinn-proto 0.11.19 and quinn-udp
0.5.16. Optional/target-specific packages in a lockfile are not evidence of
runtime exposure on every host.

Removed unused workspace declarations for `idb` and `async-stream`, and the
unused `roost-protocol` regex dependency. Coordinator `async-trait` now uses the
workspace declaration. Web Push's unused default Isahc client is disabled:
Roost builds requests through `request_builder` and sends them with its own
reqwest client. This removes curl/libz and the unused transport dependency tree;
OpenSSL remains necessary for Web Push encryption.

Sources: [Tokio changelog](https://github.com/tokio-rs/tokio/blob/master/tokio/CHANGELOG.md),
[str0m changelog](https://github.com/algesten/str0m/blob/main/CHANGELOG.md),
[sysinfo changelog](https://github.com/GuillaumeGomez/sysinfo/blob/main/CHANGELOG.md),
[protox changelog](https://github.com/andrewhickman/protox/blob/main/CHANGELOG.md),
[wasm-bindgen changelog](https://github.com/wasm-bindgen/wasm-bindgen/blob/main/CHANGELOG.md),
[Rust 1.100](https://blog.rust-lang.org/2026/11/12/Rust-1.100.0/).

## Terminal fork: retain the fork, track published releases

The base is the exact crates.io **rio-vt 0.5.32**, not unreleased main.
[`third_party/rio_vt/ROOST-PATCHES.md`](../third_party/rio_vt/ROOST-PATCHES.md)
records each retained hunk and its reason. Removing the fork would regress
pending-wrap handling, scrollback preservation, DECSTBM cursor semantics, prompt
marks, diagnostics, wasm portability or Roost's OSC contracts.

Upstream 0.5.32 includes useful bounded parsing/graphics behavior and OSC 7501
support, but upstream OSC 7501 is not a drop-in replacement for Roost's complete
contract. R8 retains the required validation, framing, query and reset behavior.
R9 restores OSC 1337 `SetUserVar`, which upstream removed but Roost consumes.

Unreleased main was 21 commits ahead at
`d07eeb78a89e8ab22fc7ec3a9e2bdaeea3680b00`; split-UTF-8 and reflow-cursor fixes
are relevant candidates for a subsequent published update. They were not mixed
into this exact published-version rebase.

Sources: [0.5.28 → 0.5.32](https://github.com/raphamorim/rio/compare/v0.5.28...v0.5.32),
[synchronized output](https://github.com/raphamorim/rio/pull/2000),
[OSC 7501](https://github.com/raphamorim/rio/pull/2039),
[OSC opt-in](https://github.com/raphamorim/rio/pull/2040),
[published release → audited main](https://github.com/raphamorim/rio/compare/v0.5.32...d07eeb78a89e8ab22fc7ec3a9e2bdaeea3680b00).

## Current direct libraries retained

| Concern | Current versions |
| --- | --- |
| RPC/protobuf runtime | connectrpc 0.9.1, connectrpc-build 0.9.0, buffa 0.9.2 |
| HTTP/database/WebSocket | axum 0.8.9, reqwest 0.13.5, rustls 0.23.46, sqlx 0.9.0, tokio-tungstenite 0.30.0, web-push 0.11.0 |
| UI | Dioxus 0.8.0-alpha.3, tw_merge 0.1.22, icons 0.19.0 |
| Terminal/media | portable-pty 0.9.0, image 0.25.10, qrcode 0.14.1, rqrr 0.11.0, unicode-segmentation 1.13.3 |
| Encoding/identity | serde 1.0.229, ed25519-dalek 3.0.0, base64 0.23.1, hex 0.4.3, ipnet 2.12.2, url 2.5.8 |
| Runtime/plumbing | futures/futures-util 0.3.34, async-trait 0.1.92, regex 1.13.1, tracing 0.1.44, tracing-subscriber 0.3.23 |
| Errors/CLI/tooling | thiserror 2.0.21, anyhow 1.0.104, clap 4.6.7, chromiumoxide 0.9.1, cargo_metadata 0.23.1 |
| Platform/files | getrandom 0.4.3, tempfile 3.27.0, uds_windows 1.2.1, windows-sys 0.61.2 |

Registry version source: crates.io's per-crate API, for example
[`connectrpc`](https://crates.io/api/v1/crates/connectrpc),
[`sqlx`](https://crates.io/api/v1/crates/sqlx),
[`dioxus`](https://crates.io/api/v1/crates/dioxus).

Dioxus 0.8.0-alpha.3 is already the latest prerelease; stable 0.7.10 would be a
downgrade. Its placement patch remains necessary. Upstream main renames the VDOM
crate and is not a compatible patch-level upgrade. Dioxus CLI stays aligned at
0.8.0-alpha.3. Its bundled Tailwind pin is 4.1.5; upstream Tailwind is 4.3.5.
An independently distributed Tailwind binary is not introduced here.

Rust/UI's pinned recipe revision remains
`7120636e68556ddd023a4ec8e083663c7b745318`: the 17 audited commits up to
`4f5a516e76d75a15a587ab2169352cf34fd318d0` do not change registry component
recipes. Repinning would not improve the shipped components.

New wasm-bindgen APIs worth considering when a real consumer needs them:
typed Promise rejection, explicit nullable values, numeric-slice transfer
modes and `JsValue::from_utf8_lossy`. No performance claim is made without a
measured Roost hot path.

Sources: [Dioxus comparison](https://github.com/DioxusLabs/dioxus/compare/v0.8.0-alpha.3...main),
[CLI Tailwind owner](https://github.com/DioxusLabs/dioxus/blob/v0.8.0-alpha.3/packages/cli/src/tailwind.rs),
[Tailwind 4.3.5](https://github.com/tailwindlabs/tailwindcss/releases/tag/v4.3.5),
[Rust/UI comparison](https://github.com/rust-ui/ui/compare/7120636e68556ddd023a4ec8e083663c7b745318...4f5a516e76d75a15a587ab2169352cf34fd318d0).

## Remaining security advisories: explicit upstream-only policy

An OSV batch query of **672 resolved registry packages** after the refresh
reported these three advisory IDs, down from 13 before the refresh. This is a
lockfile scan, not proof of exploitability or a complete security review.
The operator chose to avoid additional forks and retain these documented
upstream constraints.

| Remaining package | Advisory | Why an ordinary update cannot resolve it |
| --- | --- | --- |
| libsqlite3-sys 0.37.0 | [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459.html) | SQLx 0.9's SQLite binding excludes 0.38 and later; the fixed series starts at 0.39. Exposure requires opening an attacker-controlled database with a crafted sibling WAL. Static SQLite linkage remains required. |
| crypto-bigint 0.5.5 | [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html) | Web Push → jwt-simple 0.12 → elliptic-curve 0.13 pins the old integer family. Fixed 0.7.6+ cannot satisfy it. VAPID uses secret signing arithmetic; practical side-channel exploitability is not established. |
| rsa 0.9.10, transitive only | [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) | The Web Push JWT/crypto dependency tree still selects it. Roost's direct Cloudflare Access verifier is on 0.10 and uses public keys; do not infer private-key exposure from the verifier. |

Constraint evidence: [SQLx SQLite dependencies](https://crates.io/api/v1/crates/sqlx-sqlite/0.9.0/dependencies),
[Web Push manifest](https://docs.rs/crate/web-push/0.11.0/source/Cargo.toml.orig),
[elliptic-curve 0.14.3 dependencies](https://crates.io/api/v1/crates/elliptic-curve/0.14.3/dependencies).

## Build tooling

Checkout is updated to 7.0.1, artifact upload/download to 7.0.2/8.0.2, Docker
Buildx/login/metadata/build-push actions to 4.4.1/4.8.0/6.2.0/7.4.0. Zig and
cargo-zigbuild update to 0.17.1/0.24.1; the glibc 2.28 floor and binary-import
checks remain. Existing moving major refs for rust-cache, install-action,
setup-zig and setup-nasm continue selecting their maintained major releases.

macOS CI and both macOS release builds use `macos-15-intel`; Apple's SDK
cross-links the Apple Silicon target. This keeps both release architectures
without depending on the ARM64 hosted pool that
[failed to allocate a runner](https://github.com/cefege/roost/actions/runs/43957626879).

Quickstart consumes the self-link command's exit status, as Rust 1.100 requires,
and returns a non-success status before printing completion or pairing a browser.

PostgreSQL remains on the supported 17 major. Moving production to 18 is a
database upgrade with backup/restore implications, not a library bump.

Sources: [checkout](https://github.com/actions/checkout/releases/tag/v7.0.1),
[artifact upload](https://github.com/actions/upload-artifact/releases/tag/v7.0.2),
[artifact download](https://github.com/actions/download-artifact/releases/tag/v8.0.2),
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild/releases/tag/v0.24.1),
[Zig releases](https://ziglang.org/download/index.json).

## Rust/UI findings repaired

- Base, accent and appearance mode now synchronize through the existing account
  client-state LWW mechanism. Remote merges redraw without publishing another
  local edit. Palette synchronization does not remove responsive mobile layout.
- Input, Textarea and SidenavInput follow the existing short-side compact hook,
  retaining readable phone sizing in landscape.
- Hover scrollbars respond to their actual container instead of an absent group.
- Narrow design specimen grids cap minimum tracks at their available width.
- The whole closed mobile sidebar is inert, not just its folder list.
- Compact settings categories use stock Item title/description layout rather
  than clipping two lines inside a fixed-height sidenav button.
- Attachment actions wrap onto a separate row on narrow layouts.
- Terminal keypad Enter uses the stock primary button variant; latch selection
  uses accent roles, and its card retains the stock frame.

The mandatory design review found no introduced primitive bypasses, new raw
visual values or role misuse. Runtime verification is recorded with the change,
not inferred from the static review.

## Verification

- 186 scoped nextest cases passed across the protocol, client core, terminal,
  worker, coordinator and host crates. Coverage includes two-device theme
  merges, offline edits, legacy theme seeding, real peer connections,
  attachment transport, RS256 verification and terminal stream fences.
- The vendored Rio suite passed all 641 tests.
- All 12 native web theme-engine cases passed.
- Scoped native and wasm clippy checks passed. Repository formatting and
  structural lint passed; lint checked 7,463 inputs with zero violations.
- Managed Chromium exercised the actual UI components through a temporary
  fixture, removed after verification: desktop/mobile palette parity,
  remote → local → remote redraw, Light/System mode and OS appearance changes,
  compact settings navigation, wrapped attachment actions, closed-drawer focus
  exclusion, and sheet/TV-tray keypad rendering and modifier latches.
- Input, Textarea and SidenavInput measured 16px at 844×390 and 14px at
  1024×850. Gallery and attachment surfaces had no page-level horizontal
  overflow at 320px. These are Chromium viewport checks, not physical iOS or
  Android device tests.
- CLI clippy and 19 existing quickstart, self-link and real-coordinator API
  cases passed after adapting to Rust 1.100's must-use exit statuses.
