# Porting conventions — TypeScript module to Rust crate

You are porting one or more modules of Roost v2 (Bun + TypeScript) into
Roost v3 (Rust), on the `v3` branch at `/home/almalinux/repos/roost-v3`.
This file is the shared contract for every porting task. Read it in full
before you write a line of Rust.

## What you are and are not editing

You own a fixed, disjoint set of files under `crates/roost-protocol/src/`
(or `crates/roost-observability/src/`, `crates/roost-platform/src/`,
`crates/roost-host/src/` — your task says which). Another agent owns every
other file in the same crate.

- **Never edit** `crates/roost-protocol/src/lib.rs`. It declares the module
  tree; it belongs to the integrator.
- **Never edit** a `mod.rs` other than the one your task names. The `mod.rs`
  files already declare every module and re-export every public symbol; if a
  symbol you need is not re-exported, put it behind a `pub` in your own file
  and say so in your report.
- **Never add, remove or rename a crate dependency.** If you need one, say so
  in your report and let the integrator wire it.
- **Never edit a TypeScript file.** The TypeScript tree stays until its Rust
  replacement passes the phase gate, so the mixed stack keeps building.
- **Do not run `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, or
  `bun`.** Other agents are working in the same target directory and the Cargo
  lock serializes them; the integrator runs every gate once at the end. You
  verify by reading, not by building.

## Read before you write

For each module you port:

1. The TypeScript source.
2. Its tests, wherever they live under the package's `tests/` directory. A
   test that pins a real behaviour is a specification you must satisfy. A test
   that asserts on source text, on message wording, or on an incidental
   default is not a specification — do not port it, and say so in your report.
3. `docs/FAILURE-INDEX.md` — grep it for the module's file name and for its
   exported symbols. For every entry whose **Guard** line names a TypeScript
   test or lint that covers a module you are porting, the ported code needs
   the equivalent guard, and your report must name the entry so the Guard
   line can be rewritten to the Rust path.

## Crate rules

`roost-protocol` is pure logic. It must not read a clock, touch the
filesystem, spawn a socket, read an environment variable, or name a
platform.

- Where the TypeScript defaulted to `Date.now()`, `performance.now()`, or
  `Bun.nanoseconds()`, the Rust takes the timestamp as a required parameter.
  There is no default and no global clock, and there is no `static mut` or
  lazy global to stand one in.
- Where the TypeScript read `crypto.subtle` or `crypto.getRandomValues`, the
  Rust takes the bytes as a parameter, or the pure half becomes a function
  over already-computed bytes.
- The crate compiles for `wasm32-unknown-unknown`. A `std::time::SystemTime`
  in a signature is a bug even though it compiles.

`#![forbid(unsafe_code)]` is already set in the crate root. Do not add
`unsafe`, and do not add `#[allow(forbid(unsafe_code))]`.

## Zod becomes serde plus explicit validators

There is no Zod in Rust. The pattern is:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Session {
    pub id: SessionId,
    pub worker_fp: WorkerFp,
    pub channel: ChannelId,
    pub kind: SessionKind,
    pub cwd: String,
    pub spawn_cwd: String,
    pub workspace_id: Option<WorkspaceId>,
    pub status: SessionStatus,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    pub custom_title: Option<String>,
}

impl Session {
    /// The `Session.parse` equivalent. `value` is the already-decoded JSON.
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> { … }
}
```

- **`rename_all = "snake_case"`** on every wire type. The JSON on the wire is
  snake_case; a camelCase field silently stops matching every producer.
- **`Option<T>`** for every field that is nullable or optional in the
  TypeScript. `null` and absent are the same on the wire for these fields, so
  one `Option` is correct — but check the TypeScript for the exceptions where
  absent and null are *distinguished* (there is one, on `git.remote`) and
  preserve that distinction with a nested `Option` or a dedicated
  `present`-flagged type.
- **Enums with a fallback.** A TypeScript `z.enum([...])` becomes a Rust
  `enum` only if you can also carry an unknown value, because a newer peer
  must not break an older build's decoding. Where the wire encodes the value
  as a plain `string` — and most of this contract does — model it as a
  `String` plus a `one_of` validator, or as an enum with an `Unknown(String)`
  variant. Never map a string onto a closed Rust enum without a fallback.
- **Cross-field rules** that Zod expressed as `.superRefine()` become
  ordinary functions in the `parse` implementation, in the same order Zod
  ran them, and return `ProtocolResult`.
- **Recursion** that Zod did with a recursive schema becomes a worklist
  (`Vec` pop) in Rust, not recursion, so a deep document cannot exhaust the
  stack. Where the TypeScript capped depth at 32, keep the cap.

## Errors

`crates/roost-protocol/src/error.rs` is the only error type:

```rust
use crate::{ProtocolError, ProtocolResult};

validate::non_empty("session.cwd", &cwd)?;
Err(ProtocolError::new("snapshot.chunks", "must not be empty"))
```

`ProtocolError::within(prefix)` prefixes a field path when one validator
delegates to another. `crates/roost-protocol/src/validate.rs` has the shared
checks — `uuid`, `hex_of_len`, `integer_in_range`, `nonnegative`, `non_empty`,
`max_utf8_bytes`, `one_of`, `all_or_none`. Use them instead of re-implementing
the comparison; add a new helper there only if the rule is genuinely new, and
if you do, say so in your report.

The field path is the dotted path of the offending value, e.g.
`event.sessions[2].spans[0].columns`. It goes straight into a log line, so it
has to identify the field.

## Naming

- `snake_case` functions and fields; the same words as the TypeScript export,
  converted: `foldEvent` → `fold_event`, `parseLayoutDocumentV1` →
  `parse_layout_document_v1`, `TERMINAL_MAX_COLS` → `TERMINAL_MAX_COLS`.
- No single-letter names except `idx` in a tight loop.
- No `handle`, `process`, `do`, `manage`, or `run` alone — name the actual
  verb: `apply_delta`, `reassemble_snapshot`, `encode_fragment`.
- No `Utils`, `Helpers`, `Common`, or `Models` modules.
- A `pub fn` whose only purpose is to let a test reach a private step takes a
  leading underscore, matching the marker this repo already uses in TypeScript.

## Comments

- A `//!` header of 3–6 lines at the top of every non-trivial file: what this
  file owns, what calls it, what it depends on.
- Inline comments explain **why**, never what. Write one only when removing
  it would mislead the next reader — most often to name an invariant, or the
  incident that constrains an ordering. A TypeScript comment that explains a
  non-obvious invariant is worth carrying across nearly verbatim; a comment
  that restates the code is not.
- **No narrative comments.** No "ported from x.ts", no "added in phase 1", no
  "for now". If the reason a line exists is non-obvious, describe the
  behaviour, not the lineage. Git history is the lineage.
- Do not keep a TypeScript comment that is a *narrative* about the original.

## Size and tests

- **≤400 lines per file, counting tests.** `cargo xtask lint` fails a file
  over the cap against `xtask/file-size-baseline.json`, which starts empty
  and may only shrink. A file that would exceed the cap must move its tests
  into `crates/roost-protocol/tests/<name>.rs` — that directory is not capped,
  and the integrator has not created it yet, so create it.
- Unit tests for pure helpers go in a `#[cfg(test)] mod tests` at the bottom
  of the file. Behaviour tests that need several modules go in
  `crates/roost-protocol/tests/<name>.rs`.
- A test earns its place by catching a plausible consumer-visible bug:
  behaviour, boundaries, invariants, transitions, precedence, errors. A test
  that asserts a copy round-trips, that a message is non-empty, that a
  constant equals itself, or that the code does not panic, earns nothing.
- Deterministic and isolated. No clock, no filesystem, no network, no
  environment. Tests may not depend on execution order or on each other.

## What the deliverable looks like

Your report must state, in this order:

1. Every file you wrote or changed, with its final line count.
2. Every exported symbol you added, with its exact Rust signature.
3. Every TypeScript export you deliberately did **not** port, with the reason
   (unreachable in an all-v3 fleet, Windows-only, a capability fallback every
   v3 peer advertises, and so on). Silence here reads as "I forgot".
4. Every `docs/FAILURE-INDEX.md` entry whose **Guard** covers code you ported,
   with the entry's `###` heading and the Rust path that now guards it.
5. Every TypeScript test you did not port, with the reason.
6. Anything the integrator has to do: a new dependency, a `mod.rs` re-export,
   a constant that belongs in `versioning.rs`.
