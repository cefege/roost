# Roost vendoring record

## Source and ownership

- Base: [`revpidev/rpi@7ee970aad1d825b30b9413ce251c98e610214e2b`](https://github.com/revpidev/rpi/tree/7ee970aad1d825b30b9413ce251c98e610214e2b), `v0.1.7`.
- Base Durable lineage: official Pi `abe508e1b89912adde45528136c3221eb69acdd7`, `packages/durable` 1.1.0.
- Behavioral reference: [`earendil-works/pi@d5b3de22d0e24a3e9c38a9410d3329b099717283`](https://github.com/earendil-works/pi/tree/d5b3de22d0e24a3e9c38a9410d3329b099717283/packages/durable). Its manifest says 1.4.0; the published npm 1.4.0 artifact is not the same source.
- MIT; [`LICENSE`](LICENSE) is copied from the base revision. Included crates: `rpi-durable`, `rpi-ai`, `rpi-codemode`, and the test-only `rpi-test-env`. Code-mode embeds the base's licensed QuickJS-WASM artifact and runs it through Wasmtime; no second JavaScript engine is retained.

This is a maintained local fork, not an unmodified upstream dependency. Roost owns the patches below and must rerun its differential oracle when changing the reference or engine. No CLI/TUI, Node agent host, or upstream OAuth/account store is part of the native Roost runtime.

## Packaging and adapters

- Standalone workspace pins the libraries' dependencies without inheriting either monorepo. SQLite uses the same native SQLite linkage as Roost's SQL backend; its statement-cache feature is explicit.
- Retained the AI catalog/generated sources and required auth fixtures. Removed monorepo placement/source-text guards, not persistence behavior tests.
- Roost's native provider adapter resolves availability through its credential-aware API-key auth handler; actual requests remain on Roost's account pool, OAuth, transport and failover.
- Internal request origins distinguish generation and compaction for native policy, usage and streaming. Rich messages retain image order, signed thinking, text/tool signatures and structured results.
- `ToolHooks::on_interrupted` decorates an unsafe tool's recorded recovery failure without replaying the effect or invoking normal completion hooks.

## Durable behavior patches

- Persisted sleeping states/deadlines, host wake timers and task checkpoints; submission admission and task state can settle in one transaction.
- Read-only observation, deferred document persistence, live-versus-stored revisions, sealed close, first-cause storage failure and guarded storage throughout the harness.
- Mutation-line ordering for timer flushes and document unload; invocation identity fencing, sleeping reservation lifetime and panic settlement.
- Task records serialize their state as the official nested object; read-only scheduling is rejected at the Harness boundary before transaction admission.
- Nested tool identities, keyed replay, caller filters, structured output/errors, receipt summaries, diagnostics/usage bounds and deferred tool results.
- Code-mode uses the official frozen sandbox helpers: `tools`, `ALL_TOOLS`, `console`, `text`, `image`, `store`, `load` and `exit`, not Node globals. Durable nested-call receipts, VM fingerprints, virtual time/random inputs and issued-call acknowledgements protect replay.
- Nested receipts remain in their canonical documents and live slots; the kernel does not invent a nested-call summary on a generic root model tool result.
- Generation prepare/request/retry/poll/tool phases, pinned deferred handles, request hooks and cancellation partial settlement; manual, threshold, overflow and background compaction tasks.

The development-only oracle lives in [`../../tools/pi-durable-conformance`](../../tools/pi-durable-conformance). Its checked-in traces identify the official source revision and retain content, usage, errors, identities and ordering; a test count is not a claim of exhaustive equivalence.
