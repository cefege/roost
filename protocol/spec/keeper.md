<!-- Keeper contract: the framed socket between one worker and the long-lived PTY-owning daemon. -->
<!-- The wire format is fixed by this document; tags are never renumbered. -->

# Keeper protocol

## Purpose

A single keeper process owns N PTYs and outlives the worker that spawned it, so a coordinator deploy or a worker restart never disturbs a live terminal. Every frame carries a channel id, which is what lets one socket carry many PTYs and what lets a fresh worker find the channels that survived it.

The socket is the only path to a PTY. Nothing else in the system opens, reads, or writes one, which is what makes "the keeper holds the terminal" a property rather than a convention.

## Wire format

```text
[4-byte BE u32 total_length]  covers every byte after this field
[1-byte     type tag]
[2-byte BE u16 channel_id]    0 = control/global
[payload bytes]
```

A decoder reads from a stream, so it must handle a frame split across reads and several frames in one read. `total_length` is bounded by `KEEPER_MAX_MUX_FRAME_BYTES` (16 MiB); a length above that is a protocol violation, not a large frame, and closes the connection.

Scalars inside binary payloads are big-endian and unaligned. A `u64` sequence is `writeSequence`/`readSequence` in v2: eight bytes, most significant first.

## Tags

Stable, never renumber. A tag that changes meaning breaks every deployed keeper at once, so a new meaning takes a new tag.

| Tag | Name | Direction | Payload |
| ---: | --- | --- | --- |
| `0x10` | `Spawn` | client → keeper | JSON `{channel_id, cols, rows, shell_spec}` — open a new PTY |
| `0x11` | `SpawnAck` | keeper → client | JSON `{channel_id, pid}` |
| `0x12` | `SpawnErr` | keeper → client | JSON `{channel_id, error}` |
| `0x20` | `PtyIn` | client → keeper | raw input bytes (legacy, unacknowledged) |
| `0x21` | `PtyOut` | keeper → client | raw output bytes |
| `0x22` | `PtyInRequest` | client → keeper | `[input_seq:u64][bytes]` |
| `0x23` | `PtyInAck` | keeper → client | `[input_seq:u64][written:u32]` |
| `0x24` | `PtyInReject` | keeper → client | `[input_seq:u64][written=0:u32][reason:u8]` |
| `0x25` | `PtyInAmbiguous` | keeper → client | `[input_seq:u64][written:u32][reason:u8]` |
| `0x30` | `Resize` | client → keeper | JSON `{cols, rows}` (legacy) |
| `0x31` | `KillChild` | client → keeper | empty — terminate the PTY child |
| `0x32` | `Exit` | keeper → client | JSON `{exit_code: number \| null}` |
| `0x33` | `ResizeRequest` | client → keeper | `[seq:u64][cols:u32][rows:u32]` |
| `0x34` | `ResizeAck` | keeper → client | `[seq:u64][cols:u32][rows:u32]` |
| `0x35` | `ResizeReject` | keeper → client | `[seq:u64][reason:u8]` |
| `0x36` | `ResizeStatus` | client → keeper | `[seq:u64]` — cached-status query |
| `0xE0` | `ListChannels` | client → keeper | empty (`channel=0`) |
| `0xE1` | `ListChannelsResp` | keeper → client | JSON `{channels: [{channel_id, pid}]}` |
| `0xE2` | `Hello` | client → keeper | capability-bearing hello (`channel=0`) |
| `0xE3` | `HelloResp` | keeper → client | contract + process/channel observation |
| `0xE4` | `GetHistory` | client → keeper | empty, per channel (legacy drain) |
| `0xE5` | `GetHistoryResp` | keeper → client | `[head_seq:u64][ring bytes]` |
| `0xE6` | `GetHistoryRecords` | client → keeper | empty, per channel |
| `0xE7` | `GetHistoryRecordsResp` | keeper → client | ordered `Output`/`Resize` records |
| `0xE8` | `Shutdown` | client → keeper | empty (`channel=0`) — deliberate offline maintenance |
| `0xE9` | `ShutdownAck` | keeper → client | empty (`channel=0`) |
| `0xEA` | `GetTerminalState` | client → keeper | empty, per channel |
| `0xEB` | `GetTerminalStateResp` | keeper → client | authoritative resize state |
| `0xEC` | `ShutdownIfEmpty` | client → keeper | empty (`channel=0`) — automatic boot replacement |
| `0xED` | `ShutdownIfEmptyAck` | keeper → client | empty (`channel=0`) |
| `0xEE` | `ShutdownIfEmptyReject` | keeper → client | empty (`channel=0`) |
| `0xF0` | `Ping` | both | empty (`channel=0`) |
| `0xF1` | `Pong` | both | empty (`channel=0`) |

## Version

`KEEPER_PROTOCOL_VERSION = 3`. Bump it when an existing frame's JSON shape changes, when a frame's encoding changes, or when a tag is reassigned (never — add a tag). Backwards-compatible additive tags may instead be feature-negotiated.

A bump makes a running keeper incompatible. `Hello` still authenticates across a version mismatch so administrative tooling can report or shut the keeper down, but the worker must not dispatch application commands across the mismatch.

Bump log:

1. Initial `Hello`/`HelloResp` handshake (2026-06-18).
2. Authenticated `Hello`, `ShellSpec` spawn, ordered history, typed IO.
3. `KeeperContractV1` process/binding proof, conditional empty shutdown.

## Feature negotiation

`Hello` carries a feature list; the keeper answers with the subset it supports. A client treats the intersection as the capability set and must not send a frame whose feature was not negotiated.

Supported: `ordered_history_v1`, `acknowledged_input_v1`, `acknowledged_resize_v1`, `terminal_state_v1`.

Required is narrower than supported. `terminal_state_v1` is not required because resize can fall back to the last-written sequence probe. The rest are required because without them a surviving keeper is unusable, and boot may retire an incompatible keeper **only** after both coordinator sessions and keeper bindings prove empty.

## Why each acknowledged family exists

`PtyIn` writes and forgets: a short write, a full queue, or a dropped connection all look the same to the client, so a keystroke can be silently lost. `PtyInRequest`/`Ack`/`Reject`/`Ambiguous` name the sequence, say how many bytes reached the PTY, and distinguish "not written" from "written and then lost", which is the difference between a retry that is safe and one that duplicates a character.

`Resize` has the same problem with a worse failure: a lost acknowledgement leaves the worker unable to prove which sequence the keeper consumed, and because retained history markers can be evicted, there may be no marker left to ask about. `GetTerminalState` exists for exactly that case — the keeper answers from live channel state rather than from a record the worker may no longer be able to match.

`Shutdown` is the deliberate offline maintenance path. `ShutdownIfEmpty` is the automatic boot-replacement path, and its keeper-side channel check is atomic with admitting the shutdown: a keeper that has just been handed a new PTY must not retire itself.

## Limits

| Constant | Value |
| --- | ---: |
| `KEEPER_MAX_MUX_FRAME_BYTES` | 16 MiB |
| `KEEPER_MAX_INPUT_BYTES` | 64 KiB |
| `KEEPER_MAX_TERMINAL_DIMENSION` | 65535 |
| `KEEPER_MAX_HISTORY_RESIZE_RECORDS` | 4096 |

A dimension outside the maximum is rejected rather than clamped: a clamp silently produces a PTY whose geometry differs from what the client believes, and the client has no way to discover that.
