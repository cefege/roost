<!-- Program status contract: OSC 7501 reports as Roost parses, retains, and fans them out. -->
<!-- The sequence is defined by the public spec; this file records Roost's implementation and its gaps. -->
<!-- Canonical parser limits live in third_party/rio_vt/src/performer/program_status.rs. -->

# Program status (OSC 7501)

## Purpose

A program running in a Roost terminal reports its state (`idle`, `working`,
`done`, `blocked`, `error`) as an OSC 7501 sequence in its own output. Roost
retains the record set per channel in the worker and fans it to browsers
through Sync. The sequence is defined by the public spec
(<https://www.superlogical.com/rex/docs/build/program-status>, rev 0.4).
This file is the Roost-side contract: which parts of the spec are
implemented, where, and where the implementation departs from it.

## Surfaces

| Surface | Carrier | Owner | Source |
| --- | --- | --- | --- |
| Program output | `ESC ] 7501 ; <pairs> ST`, and the feature query `ESC ] 7501 ; ? ST` | The program that writes to the PTY | `third_party/rio_vt/src/performer/handler.rs` (`b"7501"` arm); `third_party/rio_vt/src/performer/program_status.rs` |
| Worker signal | `WTerminalSignals` fields 16–17 (`program_status_changed`, `program_status`) | Worker `ChannelSignals` | `protocol/proto/roost/v1/worker_transport.proto`; `crates/roost-worker/src/session/terminal_metadata/signals.rs` |
| Sync frame | `TerminalSignalsFrame` fields 10–11 (`program_status_present`, `program_status`) | Coordinator `SignalHub` | `protocol/proto/roost/v1/sync.proto`; `crates/roost-coord/src/terminal_screen/signal_hub.rs` |
| Client store | `TerminalSignals::program_status(session_id) -> &[ProgramStatusRecord]` | `roost-client-core` | `crates/roost-client-core/src/store/terminal_signals.rs` |

`ProgramStatusRecord` (`sync.proto`, `worker_transport.proto`) carries
`id` (optional), `state`, `kind` (optional), `app` (optional), `title`
(optional), `msg` (optional), and `progress` (optional `uint32`). The
domain type is `crates/roost-protocol/src/program_status.rs`
(`ProgramStatusRecord`, `ProgramStatusTable`, `ProgramState`, `ProgramKind`).

## Parsing

The parser lives in `third_party/rio_vt/src/performer/program_status.rs` and
runs on every OSC 7501 the core sees. The result is a `ProgramStatusReport`
carried as `RioEvent::ProgramStatus` (`third_party/rio_vt/src/event/mod.rs`).

| Rule | Behaviour | Source |
| --- | --- | --- |
| Framing | `body.len() + 9 > 4096` rejects the whole report. The body excludes `7501;`. | `program_status.rs` (`SEQUENCE_MAX_BYTES`, `FRAMING_BYTES`) |
| Pairs | Split on `:`. Whitespace around key and value is trimmed. | `program_status.rs` |
| Key | Lowercase ASCII, at most 16 bytes. A bad key skips the pair. | `KEY_MAX_BYTES` |
| Value | `[A-Za-z0-9_.,+/=-]`. A bad value skips the pair. | `program_status.rs` |
| `state` | Required. Unknown or missing: whole report `None`. `clear` maps to a clear, not a state. | `program_status.rs` |
| `id` | Grammar in the spec. Invalid: whole report `None`. At most 128 bytes, 32 per segment, 8 levels. | `ID_MAX_BYTES`, `SEGMENT_MAX_BYTES`, `ID_MAX_LEVELS` |
| `kind` | Kept only with `blocked`. Unknown value: absent. | `program_status.rs` |
| `progress` | Kept only with `working` or `blocked`; integer `0`–`100`. Otherwise absent. | `PROGRESS_MAX` |
| `app` | One segment `[A-Za-z0-9_.+-]{1,32}`. Otherwise absent. | `program_status.rs` |
| `title` | Base64. Encoded at most 256 bytes, decoded at most 192. Over-cap, bad base64, or non-UTF-8: the whole report is discarded. | `program_status.rs` |
| `msg` | Base64. Encoded at most 2732 bytes, decoded at most 2048. Same discard rule as `title`. | `program_status.rs` |
| Control character | Any `char::is_control` in decoded `title` or `msg`: the whole report is discarded. | `program_status.rs` |
| Unknown key | Ignored. | `program_status.rs` |

## Records

The record set is `ProgramStatusTable` (`crates/roost-protocol/src/program_status.rs`).
The root record has the key `""` and is the report with no `id`.

| Operation | Behaviour | Source |
| --- | --- | --- |
| `Set(record)` | Replaces the record with the same id completely. A key the report omits is absent afterwards. The report touches the record's recency. | `ProgramStatusTable::apply` |
| `Clear { id }` | Removes the record and every record under `id/`. `Clear { id: None }` removes every record. | `ProgramStatusTable::apply` |
| `ProgramEnded` | Removes `working` and `blocked` records. Keeps `idle`, `done`, `error`. The spec allows `idle` to be dropped (`MAY`); Roost keeps it. | `ProgramStatusTable::apply` |
| Cap | 256 records per channel (`PROGRAM_STATUS_RECORDS_MAX`). Past the cap, the least recently touched record is evicted. The record just written is never evicted. | `crates/roost-protocol/src/program_status.rs` |
| `app` inheritance | A record without `app` takes it from its nearest ancestor that set one, then the root record. | `inherited_app` in the same file |
| `records()` | Returns records sorted by id, root first, with inherited `app` filled in. This is the snapshot the worker sends. | `ProgramStatusTable::records` |

The spec's sequence (`clear`, replacement, parent/child clearing, `app`
inheritance) is implemented as written. Parent/child structure carries no
meaning beyond clearing and `app` inheritance.

## Worker and coordinator flow

1. A PTY chunk is parsed by the core (`crates/roost-term/src/rio/mod.rs`,
   `RioCore::write`). `RioEvent::ProgramStatus` reaches
   `crates/roost-term/src/rio/listener.rs`, which calls
   `signal_queue.rs` `record_program_status`. `clear` becomes `Clear`; the
   other states become `Set`. `take_program_status()` drains the queue after
   each parse; `discard()` clears anything left pending.
2. `crates/roost-worker/src/session/emit_ingest.rs` takes the drained updates
   into `ChannelSignals::observe` (`terminal_metadata/signals.rs`). A change
   marks the table dirty. The flush sends the **whole** `records()` set, not
   a delta.
3. The coordinator (`signal_hub.rs` `observe_program_status`) replaces the
   retained set. An unchanged set is not republished. A changed set is
   published on `terminal_signal_bus`. A new Sync subscriber gets
   `snapshot()`; the bus is publish-on-change and is not backfilled.
4. `ChannelSignals::reassert` re-marks retained records dirty after a
   reconnect or negotiation, so a new coordinator link receives the set again.

An empty set is a valid update and clears every record for the channel.

## Lifetime

| Event | Records removed | Source |
| --- | --- | --- |
| Report `clear` | Named record and subtree, or all | `Clear` |
| OSC 133 `A` (shell prompt), any screen | `working`, `blocked` | `crates/roost-term/src/rio/prompt_marks_apply.rs` `apply_prompt_mark` (`record_program_ended`) |
| Process exit | `working`, `blocked`, `idle` (`done` and `error` stay) | `crates/roost-worker/src/session/terminal_metadata.rs` `forget_channel` sends the surviving set in a final frame; `crates/roost-coord/src/terminal_screen/signal_hub.rs` `session_closed` applies the same rule on the `closed` event |
| Full reset (`RIS`) | All records | `third_party/rio_vt/src/crosswords/mod.rs` `reset_state` sends a root `Clear` report |

## Feature query

`ESC ] 7501 ; ? ST` (or `BEL`) is intercepted in the worker's query lane,
not in the core.

- `crates/roost-worker/src/session/query_probe.rs` recognises the query.
  A partial query (a prefix of the run, within `QUERY_CARRY_MAX` = 32 bytes)
  is held as carry and answered when it completes.
- `crates/roost-worker/src/session/query_reply.rs` (`answer_queries`) appends
  `PROGRAM_STATUS_REPLY` once per complete query, in probe order.
- The reply is `ESC ] 7501 ; ? : states=idle,working,done,blocked,error : kinds=permission,question,auth ESC \`.
  The lists match the spec's fixed defaults.
- On the capture lane (`core` is `None`, a frozen geometry transaction), no
  reply is written. The query is recognised and its carry advances, nothing
  more.

The query is the only sequence whose answer leaves the terminal.

## Notifications

OSC 7501 raises no desktop notification. `handler.rs` routes it to
`set_program_status` only, and `listener.rs` routes `RioEvent::ProgramStatus`
to the signal queue only. OSC 9 and OSC 777 have their own paths and are not
part of this contract.

## Security

- Program-written text reaches the coordinator and clients only as decoded
  `title` and `msg` in the record set. A control character in either discards
  the whole report.
- The only bytes written back to a program are the feature reply, which
  contains nothing from any report.
- Records are never sent to a program. There is no read path back into the
  PTY.
- Input size is capped before decoding (encoded length is checked first), so
  base64 expansion cannot exceed the decoded cap.

