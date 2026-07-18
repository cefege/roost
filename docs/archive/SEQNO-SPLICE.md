# phase-ssb: per-byte seqno scrollback splice (Path A)

Replaces sb59-sb63 (mark + 5s timeout + IndexedDB cache + ring-size
bumps + retry overlay — all reverted or band-aids) with a single
resumable-byte-log protocol. Documented choice rationale in
`project_seqno_splice_path_a_chosen.md`.

## Invariants (target end state)

1. Every PTY output byte gets a u64 monotonic seq assigned at the
   **worker** (specifically `attachOutputClient.onOutput` in
   `apps/worker/src/session-manager.ts` — see also the diff-commit
   ssb2). NOT the keeper subprocess: we pay one process boundary of
   ordering trust (keeper streams in arrival order over the UDS, which
   is in-order by construction) for the right to keep the keeper
   protocol unchanged. Implication: a keeper-process restart while the
   worker survives would reset the byte stream from the worker's view;
   today keeper restart = session end, so the case doesn't arise. If
   that ever changes, add an epoch tag and SPA-side reset. Seq strictly
   increases per session. Wraps at 2^53 (JS `Number.MAX_SAFE_INTEGER`)
   — practical lifetime > 285 years at 1 MB/s.
2. Worker per-session ring stores `{seq, bytes}` chunks; head_seq +
   tail_seq queryable in O(1).
3. Coord firehose `bytes` frame carries seq alongside bytes.
4. SPA tracks `lastSeq` per session in memory; persisted to IndexedDB
   on tab close for refresh recovery.
5. On Terminal mount OR on any seq-gap detected in firehose stream:
   SPA calls `getScrollbackSince(session_id, lastSeq)`. Worker returns
   `{startSeq, endSeq, bytes}` covering [lastSeq+1, head_seq] OR
   `{gap: true, headSeq}` if the ring already evicted lastSeq.
6. Splice protocol (mark frame, 5 s timeout, DIR_SCROLLBACK_MARK
   wire byte, splice="pre"/"post"/"done" state machine) DELETED.
7. Multi-viewer just works: each browser tracks its own lastSeq;
   worker ring serves all.

## Phase breakdown — each commit ships end-to-end

| Phase | Scope | Files | Verify |
|---|---|---|---|
| ssb1 | Wire schema: add `seq: number` to coord-worker byte frame, firehose `bytes` frame. Add `getScrollbackSince` procedure stub returning empty for now. | `apps/shared/src/wire/coord-worker.ts`, `apps/shared/src/router.ts` | `bun test apps/shared/` |
| ssb2 | Keeper assigns seq per PtyOut frame (new u64 field in frame payload header). Bump FrameType encoding to include seq. | `apps/worker/src/keeper/protocol.ts`, `keeper/main.{ts,js}`, `keeper/client.ts` | smoke `bun_smoke.test.ts` |
| ssb3 | Worker session-manager: ring becomes `Array<{seq, bytes}>`. `appendScrollback` records seq. `getScrollback` (old) returns full ring. `getScrollbackSinceImpl(channelId, lastSeq)` added. | `apps/worker/src/session-manager.ts` | `bun test apps/worker/` |
| ssb4 | Worker upstream byte frame includes seq prefix. Coord-worker-hub forwards seq to firehose. | `apps/worker/src/ws-server.ts`, `apps/coord/src/router/coord-worker-hub.ts`, `apps/coord/src/byte-hub.ts`, `apps/coord/src/buses.ts` | smoke `integration.test.ts` |
| ssb5 | SPA tracks lastSeq. New mount calls `getScrollbackSince(0)`. Bytes handler validates seq continuity. On gap → re-request. Splice-mark protocol DELETED (mark handler, 5s timeout, splice state machine). | `apps/web/src/components/Terminal.tsx`, `apps/web/src/store/sync.ts` | humanchrome smoke |
| ssb6 | ~~IndexedDB lastSeq persistence~~. **Skipped** post-ssb3: tab-close destroys wterm scrollback, so fresh mount needs full snapshot anyway. Persisting lastSeq alone yields no scrollback recovery. Combined wterm-state persistence was tried and reverted as sb61. | — | — |
| ssb7 | Delete DIR_SCROLLBACK_MARK from wire, `emitScrollbackMark` from worker, `globalScrollbackMarkBus` from coord, `registerScrollbackMarkHandler` + `_dispatchScrollbackMark` from SPA, `getScrollback` (old) RPC. | sweep via `rg DIR_SCROLLBACK_MARK\|emitScrollbackMark\|ScrollbackMark\|getScrollback ` | full smoke. **Shipped f6004313.** |
| ssb8 | Worker unit regression for getScrollbackSince ring math (boundary cases: fresh, mid-ring, head, eviction-with-gap, eviction-no-gap, unknown channel, empty ring). Pins the seqno arithmetic so off-by-one regressions can't re-introduce a torn seam silently. humanchrome e2e (firehose-kill mid-stream + counter script) is a heavier follow-up — value vs. complexity didn't justify shipping inline; ring-math unit test catches the high-leverage failure modes. | `apps/worker/tests/session-manager-seqno.test.ts` | 9 passes ✓ |

## Smoke harness (ssb8 detail)

Counter script `for i in $(seq 1 100000); do echo "i=$i"; done`. Run
under a session, let ~5000 lines stream. Disconnect firehose WS via
`chrome_javascript`. Wait 2 s. Reconnect. Assert wterm scrollback
contains every `i=N` from 1 to current head, in order, no dupes. This
is the only acceptance criterion that proves the protocol works under
the failure mode the user reported.

## Escape hatch trigger (Path B switch)

If ssb1-ssb8 ships clean BUT a new "seam torn" symptom appears within
3 phases of work after that, STOP. Don't add ssb9/10/11. Cut over to
Path B per `project_seqno_splice_path_a_chosen.md`. Recurrence is the
signal that byte-stream replay over our 4-tier topology has a deeper
issue than a seqno fixes — switching to server-side VT parse is the
next escalation.

## L11 row already added

CLAUDE.md L11 — symptom "scrollback seam torn — duped tail / missing
chunk / 'two terminals' between history and live" → this memory.
