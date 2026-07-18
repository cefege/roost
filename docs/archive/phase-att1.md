# phase-att1 — file attachment via path-injection

STATUS: SHIPPED (att1a-att1f on `v2`). `attachFile` Connect RPC,
Terminal.tsx drop/paste binding, AttachmentChipStack, attachment-reaper
(24h TTL / 1 GB LRU) all merged. Preserved for historical context.

STATUS (original): TODO (queued, not started)
SCOPE: drag/paste/pick any file onto a terminal pane → SPA uploads bytes to the
owning worker → worker saves to `~/.roost/attachments/<session_id>/` → SPA types
the absolute path into the PTY at the current cursor position. Claude (or any
TUI) sees a normal prompt containing a path; it reads the file from disk on its
own. No claude-specific protocol; works for any session kind.

CORE INSIGHT: "attachment" is a UI fiction layered over "type a path." The
browser↔worker pipe already moves bytes both directions; we add one mutation
and one keystroke-injection path.

---

## Wire (`apps/shared/src/wire/control.ts:71`)

- new `ClientControlFrame` variant
  `save-attachment { request_id, session_id, filename: string, mime: string, bytes_b64: string }`
- bytes_b64 capped at 50 MB pre-encode SPA-side
- response uses existing `rpc-ok { data: { abs_path: string } }` envelope

## Router skeleton (`apps/shared/src/router.ts`)

- new `sessions.attachFile` mutation
  - input  `{ session_id, filename, mime, bytes_b64 }`
  - output `{ abs_path }`
- coord IMPL = browser-command + `createPendingRpc` with 30 s timeout
  (pattern at `apps/coord/src/router/sessions.ts` getScrollback)

## Worker handler (`apps/worker/src/main.ts`)

- `case "save-attachment"`:
  - `dir = path.join(os.homedir(), ".roost", "attachments", frame.session_id)`
  - `fs.mkdirSync(dir, { recursive: true, mode: 0o700 })`
  - sanitize filename: strip `/`, NUL, leading dots; truncate to 80 chars;
    preserve extension
  - `fname = ${Date.now()}-${sanitized}`
  - `fs.writeFileSync(path.join(dir, fname), Buffer.from(bytes_b64, "base64"))`
  - reply `rpc-ok { abs_path: <full path> }`

## Reaper (`apps/worker/src/attachment-reaper.ts` — new)

- on worker boot: `setInterval(sweepAttachments, 60 * 60 * 1000)`
- sweep walks `~/.roost/attachments/*/*`, unlinks `mtime < now - 24h`, rmdirs
  empty session dirs
- LRU cap: total size > 1 GB → delete oldest until under

## SPA upload primitive (`apps/web/src/lib/attachments.ts` — new)

- `uploadAttachment(sessionId, file: File): Promise<string>`
  - `FileReader.readAsArrayBuffer` → chunked base64 encode (8 KB chunks,
    `btoa(String.fromCharCode(...chunk))`) to avoid call-stack blow
  - call `trpc.sessions.attachFile.mutate(...)`
  - return `abs_path`
- size guard: throw if `file.size > 50 * 1024 * 1024`
- module-level Promise chain so serial uploads are guaranteed in drop order

## SPA drop+paste binding (`apps/web/src/components/Terminal.tsx`)

- on Terminal mount attach `dragenter`/`dragover`/`dragleave`/`drop` to
  `containerRef`; on dragenter set local `dragHover` signal → render dashed
  overlay
- `paste` listener on `containerRef`: walk `e.clipboardData.items`; for each
  `kind === "file"` call `item.getAsFile()` → enqueue
- optional toolbar button → hidden `<input type="file" multiple>` → enqueue

## SPA path injection (same file)

- track injection offset: at upload start capture `wterm.buffer.active.cursorX`
  + `cursorY` → store as `pendingInsertPos`
- render placeholder via OVERLAY div positioned over wterm cursor using
  `wterm.getCellPx()` — coral pill, text `…uploading <name>…`. DO NOT
  `wterm.write` the placeholder (corrupts PTY buffer).
- on upload resolve: `conn.sendInput(channelId, abs_path + " ")` (same path
  the keystroke handler uses)
- on upload reject: toast `attachment failed: <name>: <err>`, remove overlay

## Progress chip (`apps/web/src/components/AttachmentChip.tsx` — new)

- module-level signal
  `uploads: { id, name, bytes_total, bytes_done, state: "up" | "ok" | "err" }[]`
- chip stack rendered at App-shell or TerminalPane level (must survive tab
  switch — see [[feedback_persistent_terminal_deck]])
- 2 s auto-dismiss on "ok"

## Edge cases — handle in att1, not later

- multiple files in one drop: enqueue all; paths concat space-separated
- drop while another in flight: queue; second path injects after first resolves
- worker offline: mutation timeout → toast, no path typed
- session closed mid-upload: mutation rejects "unknown session"; chip → err;
  no path typed
- typing while upload running: lock-cursor mode — overlay marks insertion
  offset; on resolve path REPLACES placeholder at original offset, not at
  current cursor

## Out of scope (defer to att2+)

- preview thumbnails in chip
- attachment browser UI (`~/.roost/attachments/<sid>/` ls view)
- inline image rendering in wterm (sixel / image protocol)
- 23-hour expiry warning
- cross-session reuse of attachments
- automatic claude image-paste protocol detection
- symlink short-path mode (`~/.roost/attachments/<sid>/.shortcuts/p1` → real file)

## Smoke (`.claude/skills/roost-smoke/`)

- new step 10: drag a 100 KB PNG onto terminal
  - assert chip appears
  - assert chip flips to ok
  - assert `.term-row` text contains `/Users/.../attachments/<sid>/` substring
  - assert file exists at that path via worker `read-file` rpc

## Lint guard (`scripts/lint-roost.ts`)

- forbid `wterm.write(` calls inside `apps/web/src/lib/attachments.ts` or
  `apps/web/src/components/AttachmentChip.tsx` — path must go through
  `conn.sendInput`, not local buffer paint. Paint without PTY roundtrip means
  claude doesn't see the path.

## Commit shape

- phase-att1a: wire + router skeleton (`apps/shared/`, no impl)
- phase-att1b: worker save-attachment handler + reaper
- phase-att1c: coord attachFile mutation + browser-command plumbing
- phase-att1d: SPA upload primitive + drop/paste binding + path injection
- phase-att1e: progress chip + ghost overlay
- phase-att1f: smoke step 10 + lint rule
