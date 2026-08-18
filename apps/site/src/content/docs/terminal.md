---
title: "Terminal fidelity"
description: "Cell-authoritative frames, resume by sequence, on-demand scrollback, a pinned verified WASM VT core, predictive echo, mouse modes, and real hyperlinks."
order: 4
section: "Concepts"
---

## Cells, not bytes

Streaming raw PTY bytes into a browser terminal looks simple and corrupts in
practice: every window resize re-reflows history at a new width, and every
reconnect either duplicates or drops output. Roost uses the model server-side
terminal multiplexers use instead.

The **worker** holds the one authoritative grid per session and rebuilds it at a
single agreed width when the viewport changes. The **browser** renders that grid
as-is and never re-reflows it. Raw output never reaches the browser at all: the
worker sends authoritative cell-grid snapshots and deltas, and those cells are
what cross the wire.

## Resume by sequence

Delivery is resumable by sequence number. Every cell frame carries a monotonic
`seq`. A viewer's claim carries the `held_cell_seq` it has already applied, and
the worker answers a stale or unset sequence with exactly one authoritative full
frame. A splice therefore cannot duplicate or drop rows — the worst case is one
redundant full frame.

One layer lower, per-byte sequence numbers live in the keeper's per-channel ring,
which is how a restarted worker re-adopts a live PTY without losing the tail of
its output.

## History is fetched on demand

Scrollback is not streamed. It is requested explicitly, guarded by the session's
`scrollback_total`, and served from the worker's retained history. The patched VT
core retains roughly 10,000 lines per session where the stock build caps at
1,000. A single request returns at most 2,000 rows, and the worker walks them in
250-row slices with a yield between slices so one session's backfill never blocks
another session's live output for a whole frame.

When history really is missing, the response says why: genuine eviction is
distinguished from a resize-forced replay bounded by the raw ring, rather than
silently showing a short buffer.

## The VT core is pinned and verified

The core is `@wterm/core` 0.3.4, instantiated from a locally patched WASM build
committed in the repository alongside its SHA-256 digest. Loading is fail-fast:
the bytes are rehashed against that digest and every 0.3.4 bridge export is
checked by name. Either check failing throws, and worker readiness fails with it.

There is deliberately no stock-WASM fallback. Stock `@wterm/core` caps scrollback
at 1,000 lines, so a silent fallback would truncate history on exactly the
long-output sessions that need it.

## Wide glyphs are explicit

Column occupancy is stated on the wire: each cell span carries how many terminal
columns it owns. A double-width CJK ideograph or emoji is therefore one atomic
two-column span, and no phantom continuation cell is ever emitted. The scrollback
origin is read from the core's own discarded-row count rather than inferred, so
absolute history indices cannot re-alias.

## Two kinds of links

**OSC 8 hyperlinks** travel as per-cell link identity on the wire. Nothing
derives them from the byte stream and nothing matches link text — if a program
emitted a hyperlink escape, that is the link you get.

**Inferred links** are a separate layer in the renderer, applied to plain output:

- URLs, including scheme-less loopback and `localhost` addresses with an explicit
  port, which is what dev servers print;
- `owner/repo#123` and `owner/repo@<sha>` references;
- bare `#123` and bare commit SHAs, resolved against the session folder's GitHub
  origin when it has one;
- file paths, which resolve to Roost's own file viewer at
  `/file/<worker>/<path>#L<line>` instead of a browser navigation.

Links arm while you hold the platform link modifier, and a floating hint shows the
destination before you click, so a click is never a surprise. File links open the
in-app viewer, which honours the `#L42` fragment and scrolls to that line.

## The application decides what happens to the mouse

Every cell frame carries the modes the core read out of the PTY stream:
`mouse_tracking` (DECSET 1000 and 1002), `mouse_sgr` (1006), and `focus_events`
(1004). A pointer gesture is forwarded only when the running application actually
asked for it and the per-device preference is on, and it is encoded the way the
application asked — SGR-1006, or legacy X10 with coordinates clamped at 223.
Mode 1000 reports press and release only; 1002 adds motion while a button is
held. With focus events armed, the pane reports `ESC [ I` and `ESC [ O` on real
focus and blur.

Alt-screen occupancy is explicitly *not* the question. `vim`, `less`, and `man`
occupy the alternate screen without requesting the mouse, and forwarding to them
swallowed clicks with no native fallback.

## Predictive echo

On a link with real latency, each keystroke otherwise visibly trails one full
round trip. Roost draws the predicted character immediately in a client-side
overlay and reconciles it against the authoritative frame that arrives about one
RTT later. The overlay never touches the byte or cell stream and never reaches
the worker, so a wrong guess lives at most one frame.

It is gated so it costs nothing where it buys nothing. Predictions engage when
smoothed RTT/2 exceeds 5 ms (about RTT above 10 ms) and disengage, with
hysteresis, only when the link is back under a few milliseconds and nothing is
pending. A guess is underlined past roughly 160 ms RTT, and a prediction still
pending after 250 ms is shown anyway because the link has stalled. Only printable
single-width characters and backspace are predicted; a control byte, an escape
sequence, or a paste larger than 100 bytes clears every pending prediction, and
alt-screen TUIs suppress the feature entirely.

## Reading a pane is a state, not a stall

A terminal pane carries an explicit reader intent — live or reading — plus a hold
for an active selection and an armed link. Passive output and drafting in the
composer never cancel a reader, so scrolling back to read something does not get
yanked to the bottom by the next line of output. One admitted keystroke does end
it: the holds clear, the pending frame is adopted, and the bottom is re-pinned as
a single transition, so an admitted input causes at most one repair. Leaving the
surface ends the reading interval, so a pane you come back to shows the newest
canonical frame.

## Control operations are proven and bounded

A viewport change is a transaction on the worker with named phases —
validating, admitted, keeper written, PTY resized, grid rebuilt, settled — each
with its own bounded deadline under the transaction's ceiling. Only `validating`
may fail as a definite rejection; past that a failure is reported as ambiguous
unless the keeper can prove the PTY was never resized. Every result carries a
write phase, so "rejected" means *proven* untouched rather than assumed.

The timeouts nest deliberately, each inside the one outside it: the keeper's
2.5 s per-command watchdog, then the worker's relative budget, then the
coordinator's control timeout (5 s for input, 8 s for viewport), then the
browser's 10 s. An inner expiry always reports back while its outer waiter is
still listening.

## Next

- [Agents](/docs/agents/) — status detection on top of an ordinary PTY
- [Mobile](/docs/mobile/) — the same terminal on a phone
- [Fleet](/docs/fleet/) — who owns the PTY and who owns the log
- [Alternatives](/alternatives/) — how this differs from a byte-stream web terminal
