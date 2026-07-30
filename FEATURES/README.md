<!-- AUDIENCE: claude -->
# Roost features — unified inventory

Canonical registry of all shipped, in-progress, and deferred features.
Status authoritative in this table; detail files linked per row.

## Status legend
- **SHIPPED**: commits in git log; feature live
- **PARTIAL**: foundation landed; full activation gated on smoke pass
- **DEFERRED**: consciously postponed; decision documented
- **DELETED**: no longer applicable (superseded or wrong premise)

## Shipped — transport stack (2026-06)

| Feature | Scope | Design | Last commit |
|---|---|---|---|
| **phase-24** | bidir WSS per seam, worker outbound-only | ../docs/archive/phase-24.md | superseded by crpc5 (deleted raw WSS hub) |
| **phase-25** | scrollback replay + worker-restart + schema cleanup | ../docs/archive/phase-25.md | shipped |
| **phase-26** | smoke backdoor + keeper pool | ../docs/archive/phase-26.md | shipped (wsLink deferred → superseded by Connect streams) |
| **phase-ssb** | per-byte seqno scrollback splice (Path A) | ../docs/archive/SEQNO-SPLICE.md | shipped (b3aa5e93) |
| **phase-pb14/15/16** | parallel mount + mount-priority + binary scrollback | — | 5c3369b5, 7b5953a1, ef1988b6 |
| **phase-att1** | file attachment via path-injection (att1a-att1f) | ../docs/archive/phase-att1.md | shipped — `attachFile` Connect RPC + Terminal drop/paste binding |
| **CONNECT-RPC** (crpc1-crpc6) | wire collapse — Connect + protobuf binary end-to-end | ../docs/archive/CONNECT-RPC-MIGRATION.md | shipped — 0790d4da retired raw WSS hub |
| **T1.1** | drop H3 — Bun.serve native fetch handler | — | cddc9c2f |
| **T1.2** | proto-ize SessionEvent + bus deltas | — | fdf6bc32 + 5e310a4b + 2a09ba37 (all 8 variants + JsonEvent fallback retired) |
| **T1.3** | OTEL tracing on coord + worker + web | — | eae76379 + 0d3662be (W3C traceparent end-to-end) |
| **T1.4** | reconnect backfill via since_event_id | — | d2471f6a |
| **T2.1** | multiplexed keeper foundation + client + spawn flip | — | adef1fe4 + 550eb634 + be01248e (gated by `ROOST_KEEPER_MODE=multiplexed`) |
| **T2.2** | in-band JWT rotation on worker bidi | — | b766cff9 |
| **T3.1** | createCoord(deps) multi-runtime factory + Node demo | — | 894a90d3 + f5ddbb1c |
| **T3.2** | headless coord e2e test harness + bidi routing test | — | 4d55a51c + c17cb5ac |

## Deleted / superseded product modes

| Feature | Status | Replacement |
|---|---|---|
| **Structured/HTML agent mode** | **DELETED** — terminal-only cutover, 2026-07-30 | Every session is a shell PTY. Agent CLIs run inside it and keep their native terminal UI; Roost owns no agent RPC child, structured transcript, approval UI, or OMP dependency. |

## Deferred / decision gates

| Feature | Reason | Unblock |
|---|---|---|
| **T2.1 spawn default** | mux pool keeper foundation + spawn branch shipped, but ROOST_KEEPER_MODE default still legacy | smoke pass under mux mode + cross-process resume hook |
| **PLAN-NEUT** (S0→S4) | Neutralino macOS → Win → Linux | ship macOS first before multi-platform push |

## Architecture decisions (reference)

- `../docs/archive/SEQNO-SPLICE.md` — per-byte seqno scrollback splice (Path A chosen). Shipped as phase-ssb.
- `../docs/archive/PATHB-PARSER-CHOICE.md` — wterm @core parser decision (Path A seqno+ring chosen; Path B = server-side VT parse documented as escape hatch).
- `../docs/archive/PATHB-VT-PARSE.md` — Path B investigation notes (server-side VT parse model).

## Deleted plan files (no longer applicable)

- `../docs/archive/MIGRATION-STATUS.md` — superseded by this README's shipped table.
- `att2-image-media-extensions.md` — never created; wterm has no image hooks (only `getCursor()`) and the original plan was based on nonexistent APIs (`onOscSequence`, `drawImage`, etc.).
- `ROADMAP.md` — never created; its phase-24/25→Connect-RPC dependency graph was incorrect and the Connect-RPC migration shipped without that path.

---

**Last updated:** 2026-07-30
**Inventory authority:** this README + `git log --oneline` (commits are the truth)
