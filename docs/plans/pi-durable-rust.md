# Rust Pi Durable: implementation boundary

## DURABLE-TARGET

Roost uses a maintained Rust Pi Durable fork, not `bigfish1913/pi-rust` and not a Node agent sidecar. The behavioral reference is [`earendil-works/pi@d5b3de22d0e24a3e9c38a9410d3329b099717283`, `packages/durable`](https://github.com/earendil-works/pi/tree/d5b3de22d0e24a3e9c38a9410d3329b099717283/packages/durable). Its manifest reports 1.4.0; the published npm artifact is different and is not the oracle.

The reusable engine is vendored under [`third_party/pi-durable-rs`](../../third_party/pi-durable-rs), based on `revpidev/rpi` v0.1.7 / `7ee970aad1d825b30b9413ce251c98e610214e2b`. That base ported official Durable 1.1.0 / `abe508e1b89912adde45528136c3221eb69acdd7`. The [vendoring record](../../third_party/pi-durable-rs/ROOST-PATCHES.md) identifies the local behavior patches and maintenance owner.

This is **fork maintenance**, not zero maintenance. Upstream Durable changes require reviewing the source delta, updating the Rust fork and rerunning the oracle. Roost no longer keeps a second primary model/tool/compaction engine beside it. Exact OMP harness parity remains outside [the native chat contract](../../protocol/spec/agent-chat.md).

## DURABLE-RUST-CANDIDATES

| Candidate | Decision |
|---|---|
| [`revpidev/rpi`, `rpi-durable`](https://github.com/revpidev/rpi/tree/7ee970aad1d825b30b9413ce251c98e610214e2b/crates/rpi-durable) | Selected: independent async library, Tokio tasks and an adaptable async storage seam. Newer Durable behavior is maintained in the local fork. |
| [`y0usaf/pi-durable-rs`](https://github.com/y0usaf/pi-durable-rs/tree/837a9ad730e07aa8ef2e1a73764a6007dc143c5c) | Actual Durable port, but its blocking storage/execution boundary is a worse fit for the coordinator. |
| [`xiaotubaobiu/pi-rust`](https://github.com/xiaotubaobiu/pi-rust) | Contains Durable, but couples it to a monolithic CLI/TUI/provider package. |
| [`bigfish1913/pi-rust`](https://github.com/bigfish1913/pi-rust) | Its agent harness is not the requested Durable document/task substrate. Not substituted. |

## DURABLE-OWNERSHIP

| Concern | Single owner |
|---|---|
| Transactions, document history/migrations/watches, tasks, sleeping/wake, submissions, ownership, context heads/edits | Vendored `rpi-durable` |
| Generation, retries/deferred polling, tool rounds/nested calls, code-mode journaling, compaction | Vendored built-in Harness tasks |
| Native input request IDs, queues/withdrawal, run intent, leases/epochs, coordinator wake-ups | Roost `AgentRuntime` and SQL stores |
| Provider accounts, OAuth, transport, account rotation and model chains | `roost-llm`, through the trusted Durable provider adapter |
| Worker dispatch, effect receipts, file restore checkpoints and read-only enforcement | Roost worker tools and coordinator dispatch ledger |
| Goals/loop/advisor/plan/ask/todo/subagents/roles/slash commands | Roost extensions/hooks and product policies |
| Native transcript/cursors/inspection and stock Rust/UI | Roost projections and existing client APIs |

`crates/roost-agent/src/durable/` is the host adapter. The outer run supervisor enters the real Durable generation task; it does not implement another tool loop. Embedded QuickJS runs code-mode programs without introducing a production Node process.

## DURABLE-PERSISTENCE

Each native conversation has one `DurableSession` namespace. Its core root is ID 1; minted IDs start at 2 and are never reused. Core IDs, core commit sequences, native journal sequences, native conversation IDs and provider session IDs are separate domains.

Paired `0013_agent_durable.sql` migrations add confirmed kernel batches and namespace ID allocation without changing applied migration history. One fenced SQL transaction commits a kernel batch and its native transcript projections. The adapter prepares an indexed memory candidate first and adopts it only after SQL settlement; event publication is downstream of the confirmed write. A stale owner cannot allocate IDs or append batches. An owner handover replays confirmed batches into the same kernel, not into a replacement execution loop.

Existing native journals are imported as origin-tagged model contributions. Visibility follows effective rewinds and retained compaction heads; generated kernel entries are projected once and never imported back as duplicate messages. Native forks retain their materialized product history and open an independent namespace with the canonical `asOf`/`current`/`initial` conversation-document policies; historical code-mode store values follow the selected history boundary, while tasks, queues and live state start fresh. Accepted/withdrawn/consumed inputs and explicit Stop remain governed by the native command contract. Imported unsafe calls are not dispatched again.

## DURABLE-VERIFICATION

The [development oracle](../../tools/pi-durable-conformance/README.md) runs the pinned official source and independently runs the Rust engine. Checked-in traces cover plain/tool turns, context/usage, dedup/reopen, queue admission/withdrawal, document fork policies, sleep/wake, SIGKILL during an unsafe tool, ownership abort, nested keyed replay/structured errors, read-only observation and deferred live-versus-stored progress. Only declared timing/generated-provider fields are normalized; content, errors, usage, Durable IDs and sequences remain exact.

Native acceptance additionally covers SQLite/Postgres atomic projection and fences, existing-history import/rewind/fork, worker effect recovery, cancellation partials, persisted yields, model/account policies and product commands. Browser proof uses isolated coordinator/worker data and a disposable local model endpoint; no operator credentials are required. The stock UI and notification proof artifacts are under `target/stock-rust-ui/target/ui-proof/artifacts/`.

No test count establishes exhaustive equivalence or authorizes deployment. Release promotion remains a separate operator approval; changing source here does not deploy the coordinator or fleet.
