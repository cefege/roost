// roost api — headless introspection + drive over the coord Connect RPCs.
// Lets Claude Code read a terminal's grid/scrollback, list the sidebar
// (sessions/workers/workspaces), inject input, spawn/kill, manage workspaces
// + tasks, and SEE/DRIVE the browser's pane tiling (ui-state / ui verbs).
// Reuses the worker's headless auth (loadWorkerKey + mintJwt) +
// createCoordClient wholesale; adds zero coord/worker/proto code.
// Callers: apps/roost-cli/src/main.ts (the `api` subcommand).
//
// Boundary: this sees the coord DB projection + the worker's serialized grid.
// It does NOT run the SPA, so it is blind to browser DOM/render/focus — the
// offscreen-textarea focus-dead bug and client-side render corruption still
// need humanchrome / window.__smoke DOM probes.

import { basename, join } from "node:path";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";
import { createCoordClient, type CoordClient } from "../../worker/src/coord-client.ts";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { diag } from "@roost/shared/diag";
import { openSyncWs } from "./sync-ws.ts";

// Self-authorization hook, armed by buildApiClient once the key is loaded.
// A closure (not a stored key) so jwt.ts's LoadedKey stays private to that
// module. api() invokes it on an Unauthenticated dispatch: registers our raw
// 32-byte ed25519 pubkey via AuthAuthorizeBrowser — a PUBLIC endpoint gated
// loopback-or-tailnet on coord (handlers-auth.ts:47-61) — then retries once.
let _authorizeSelf: ((c: CoordClient) => Promise<string>) | null = null;

// Exported for smoke/api_smoke.test.ts (headless E2E harness contract).
export async function buildApiClient(): Promise<CoordClient> {
  const cfg = loadWorkerConfig();
  // Harness/agent override: ROOST_COORD_URL beats the worker-config
  // coordinatorUrl (which itself defaults from ROOST_COORDINATOR_URL).
  if (process.env.ROOST_COORD_URL) cfg.coordinatorUrl = process.env.ROOST_COORD_URL;
  // The worker's key is already in coord's authorized_keys → this JWT passes
  // requireAuth for free. A Mac with NO worker installed must not write into
  // the worker's support dir — fall back to ~/.roost/cli-key instead.
  // loadWorkerKey generates a fresh OpenSSH ed25519 key when the path is
  // missing (and memoizes module-level → one key per process, fine for a
  // oneshot CLI). A fresh key is unknown to coord → Unauthenticated → api()
  // self-authorizes via the hook above and retries once.
  const keyPath = existsSync(cfg.workerKeyPath) ? cfg.workerKeyPath : join(homedir(), ".roost", "cli-key");
  const key = await loadWorkerKey(keyPath);
  _authorizeSelf = async (client) =>
    (await client.authAuthorizeBrowser({
      // decodeEd25519Pubkey on coord accepts base64 of the raw 32-byte pubkey.
      sshPubkeyB64: Buffer.from(key.pubKey).toString("base64"),
      label: "roost-cli",
    })).fingerprint;
  return createCoordClient({ cfg, getJwt: () => mintJwt(key, "roost-coordinator") });
}

/** Mint a one-shot worker bootstrap token from the coord — the primitive
 *  behind `roost add-mac` / the web "Add machine" dialog. Reuses the same
 *  self-authorize-and-retry as api(): on a coord-only host the quickstart
 *  worker key is already authorized so the first attempt succeeds; a bare
 *  coord with only a fresh cli-key self-authorizes over loopback/tailnet. */
export async function mintWorkerBootstrap(label: string): Promise<string> {
  const c = await buildApiClient();
  const attempt = () => c.authMintBootstrap({ kind: "worker", label });
  try { return (await attempt()).token; }
  catch (e) {
    if (/unauthenticated/i.test(String(e)) && _authorizeSelf) {
      await _authorizeSelf(c);
      return (await attempt()).token;
    }
    throw e;
  }
}

/** Numeric flag: `--cols 200` → 200, else fallback. */
function numFlag(args: string[], name: string, fallback: number): number {
  const i = args.indexOf(name);
  if (i < 0 || i + 1 >= args.length) return fallback;
  const n = Number(args[i + 1]);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

/** String flag: `--name foo` → "foo", else undefined. */
function strFlag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  if (i < 0 || i + 1 >= args.length) return undefined;
  return args[i + 1];
}

// JSON.stringify replacer: bigint → string (proto uint64 fields), and drop
// connect-es's internal `$typeName` so agent/event dumps stay readable.
function jsonReplacer(k: string, v: unknown): unknown {
  if (k === "$typeName") return undefined;
  return typeof v === "bigint" ? v.toString() : v;
}

function requireArg(v: string | undefined, what: string): string {
  if (!v || v.startsWith("--")) {
    console.error(`roost api: missing <${what}>`);
    process.exit(1);
  }
  return v;
}

/** Current CAS version of a workspace — read via WorkspacesList (there is no
 *  point-get RPC; the list is small). Exits on unknown id. */
async function wsVersion(c: CoordClient, id: string): Promise<bigint> {
  const { workspaces } = await c.workspacesList({});
  const w = workspaces.find((x) => x.id === id);
  if (!w) { console.error(`roost api: no workspace ${id}`); process.exit(1); }
  return w.version;
}

/** CAS-guarded workspace mutation: fetch version, attempt, and on a "version
 *  mismatch" FailedPrecondition (a racing SPA bumped it between our read and
 *  write) re-read + retry ONCE. ALL mutating Workspaces RPCs require an exact
 *  if_version (handlers-workspaces.ts:135,146,159), not just update. */
async function withWsCas<T>(c: CoordClient, id: string, fn: (ifVersion: bigint) => Promise<T>): Promise<T> {
  try {
    return await fn(await wsVersion(c, id));
  } catch (e) {
    if (!/version mismatch|failed_precondition/i.test(String(e))) throw e;
    return await fn(await wsVersion(c, id));
  }
}

/** Resolve a worker by exact fp, unique fp-prefix, or exact label — so CLI
 *  callers can say `worker-rm mac-studio` instead of pasting a 64-char fp.
 *  Exits on no-match or an ambiguous prefix/label. */
async function resolveWorkerFp(c: CoordClient, arg: string): Promise<string> {
  const { workers } = await c.workersList({});
  const exact = workers.find((w) => w.fp === arg);
  if (exact) return exact.fp;
  const matches = workers.filter((w) => w.fp.startsWith(arg) || w.label === arg);
  if (matches.length === 1) return matches[0]!.fp;
  if (matches.length === 0) { console.error(`roost api: no worker matching "${arg}"`); process.exit(1); }
  console.error(`roost api: "${arg}" is ambiguous — matches ${matches.map((w) => `${w.label}(${w.fp.slice(0, 8)})`).join(", ")}`);
  process.exit(1);
}

export async function api(args: string[]): Promise<void> {
  const [verb, ...rest] = args;
  if (!verb) {
    console.error("roost api <verb>: sessions | cat | cells | input | message | rename | assign | attach | spawn | kill | workers | worker-rename | worker-rm | workspaces | ws-create | ws-update | ws-delete | ws-set-sessions | tasks | task-enqueue | task-cancel | ui | ui-state | agent | events | watch");
    process.exit(1);
  }

  let c: CoordClient | undefined;
  try {
    // Keep stdout clean for machine consumption (jq/grep on our output).
    // loadWorkerKey logs "worker key loaded" via the shared facade to stdout;
    // shunt console.log→stderr just while building the client, then restore.
    const realLog = console.log;
    console.log = ((...a: unknown[]) => console.error(...a)) as typeof console.log;
    try { c = await buildApiClient(); } finally { console.log = realLog; }
    await dispatch(c, verb, rest);
  } catch (e) {
    // Duck-type the Connect Unauthenticated error by its "[unauthenticated]"
    // message tag — avoids depending on @connectrpc/connect (a worker-only dep
    // not resolvable from the roost-cli package).
    if (/unauthenticated/i.test(String(e)) && c && _authorizeSelf) {
      try {
        const fp = await _authorizeSelf(c);
        console.error(`roost api: key unknown to coord — self-authorized as ${fp.slice(0, 8)} (label roost-cli), retrying`);
      } catch {
        console.error(
          "roost api: unauthenticated, and self-authorization was refused.\n" +
          "  AuthAuthorizeBrowser only accepts loopback or tailnet callers — run this\n" +
          "  from the coord Mac or a tailnet peer (or on a Mac with a registered worker).",
        );
        process.exit(1);
      }
      try {
        await dispatch(c, verb, rest); // retry ONCE with the now-authorized key
        return;
      } catch (e2) {
        console.error(`roost api: ${e2 instanceof Error ? e2.message : String(e2)}`);
        process.exit(1);
      }
    }
    console.error(`roost api: ${e instanceof Error ? e.message : String(e)}`);
    process.exit(1);
  }
}

async function dispatch(c: CoordClient, verb: string, rest: string[]): Promise<void> {
  switch (verb) {
    case "sessions": {
      const { sessions } = await c.sessionsList({ status: "all" });
      for (const s of sessions) {
        const agent = s.agent ? `${s.agent.status}${s.agent.stale ? "(stale)" : ""}` : "-";
        const title = s.customTitle || "";
        console.log([s.id, s.workerFp, s.kind, agent, s.cwd, title].join("\t"));
      }
      break;
    }
    case "workers": {
      const { workers, routableFps } = await c.workersList({});
      const routable = new Set(routableFps);
      for (const w of workers) {
        console.log([w.fp, w.label, routable.has(w.fp) ? "online" : "offline", w.os].join("\t"));
      }
      break;
    }
    case "worker-rm":
    case "workers-remove": {
      // Deregister a worker (WorkersDelete): drops the workers + authorized_keys
      // rows atomically — the API-side equivalent of Settings → Machines → Remove.
      const fp = await resolveWorkerFp(c, requireArg(rest[0], "fp|prefix|label"));
      const r = await c.workersDelete({ fp });
      console.log(String(r.ok));
      break;
    }
    case "worker-rename": {
      // Relabel a worker (WorkersRename) — the API-side equivalent of the
      // Settings → Machines → Rename action.
      const fp = await resolveWorkerFp(c, requireArg(rest[0], "fp|prefix|label"));
      const label = rest.slice(1).filter((a) => !a.startsWith("--")).join(" ");
      requireArg(label || undefined, "label");
      const r = await c.workersRename({ fp, label });
      console.log(r.worker?.label ?? "");
      break;
    }
    case "workspaces": {
      const { workspaces } = await c.workspacesList({});
      for (const w of workspaces) {
        console.log([w.id, w.workerFp.slice(0, 8), w.name, w.folderPath, `${w.sessionIds.length} sess`].join("\t"));
      }
      break;
    }
    case "cat": {
      // cell-phase-4: getScrollbackSince RPC retired — cell frames are the sole
      // output path. Use `cells` verb (sessionsGetScrollbackCells) instead.
      console.error("cat: removed in cell-phase-4 — use `cells` for scrollback, or `events` for live output");
      process.exit(1);
      break;
    }
    case "input": {
      const sid = requireArg(rest[0], "sessionId");
      const raw = requireArg(rest[1], "text");
      let text = raw.replace(/\\n/g, "\n").replace(/\\t/g, "\t").replace(/\\r/g, "\r");
      if (rest.includes("--enter")) text += "\r";
      await c.sessionsInput({ sessionId: sid, data: new TextEncoder().encode(text) });
      break;
    }
    case "attach": {
      // Upload local file(s) to a session's worker over the chunked
      // AttachFileChunk RPC, print each abs_path (one per line), optionally
      // inject the path into the PTY. Reference copy of the chunk loop:
      // apps/web/src/lib/attachments.ts:44-78 (uploadId/seq/last/0-byte
      // semantics). No shared extraction — the web copy carries SPA-only
      // concerns (serial queue, progress store) two ~15-line loops don't justify.
      // Usage: roost api attach <sessionId> <file...> [--inject] [--short-path]
      const sid = requireArg(rest[0], "sessionId");
      const paths = rest.slice(1).filter((a) => !a.startsWith("--"));
      requireArg(paths[0], "file");
      const inject = rest.includes("--inject");
      const shortPath = rest.includes("--short-path");
      const CHUNK_BYTES = 4 * 1024 * 1024; // keep in sync w/ web attachments.ts
      // Serial, argv order — the worker refuses out-of-order seq and this
      // mirrors the web's drop-order queue; do NOT parallelize.
      for (const path of paths) {
        const f = Bun.file(path);
        if (!(await f.exists())) throw new Error(`no such file: ${path}`);
        const uploadId = crypto.randomUUID();
        let absPath = "";
        let seq = 0;
        // ≥1 chunk always, so a 0-byte file still creates the file + returns a path.
        for (let offset = 0; offset === 0 || offset < f.size; offset += CHUNK_BYTES) {
          const data = new Uint8Array(await f.slice(offset, offset + CHUNK_BYTES).arrayBuffer());
          const last = offset + CHUNK_BYTES >= f.size;
          const res = await c.attachFileChunk({
            uploadId, sessionId: sid, filename: basename(path),
            shortPath, data, last, seq: seq++,
          });
          if (last) absPath = res.absPath;
          else console.error(`${basename(path)}: ${offset + CHUNK_BYTES}/${f.size} bytes`); // progress → stderr, stdout stays machine-clean
        }
        console.log(absPath); // stdout = one abs_path per line, nothing else
        if (inject) {
          // trailing space matches the UI's enqueueAttachment (attachments.ts:87)
          await c.sessionsInput({ sessionId: sid, data: new TextEncoder().encode(`${absPath} `) });
        }
      }
      break;
    }
    case "spawn": {
      const workerFp = requireArg(rest[0], "workerFp");
      const folder = requireArg(rest[1], "folder");
      const kind = rest.includes("--claude") ? "claude" : "shell";
      const r = await c.sessionsSpawn({ workerFp, kind, folder });
      console.log(JSON.stringify({ sessionId: r.sessionId, channelId: r.channelId }));
      break;
    }
    case "kill": {
      const sid = requireArg(rest[0], "sessionId");
      const r = await c.sessionsKill({ sessionId: sid });
      console.log(String(r.accepted));
      break;
    }
    case "agent": {
      // Full AgentState for one session (status/model/tokens/cost/currentTool/
      // permissionRequest/subAgents/stale) — sessionsList already carries it;
      // the `sessions` verb only prints .status. Debugs the agent-detection class.
      const sid = requireArg(rest[0], "sessionId");
      const { sessions } = await c.sessionsList({ status: "all" });
      const s = sessions.find((x) => x.id === sid);
      if (!s) { console.error(`roost api: no session ${sid}`); process.exit(1); }
      if (!s.agent) { console.log("no agent (shell session, or agent not started)"); break; }
      console.log(JSON.stringify(s.agent, jsonReplacer, 2));
      break;
    }
    case "events": {
      // LIVE wire-delta monitor for one session over a --secs window. Prints
      // every non-binary Sync frame referencing the session — sessionEvent
      // (lifecycle) PLUS claudeStatus/terminalTitle/lastActivity/etc. (the
      // volatile deltas that actually drive the sidebar chips). Answers "when I
      // act on this pane, what does the coord emit?" — the projection-drift /
      // "store doesn't reflect variant" debugger. Run it, then in another shell
      // `input`/`kill`/resize the session and watch the deltas land.
      //
      // Live-only (sinceEventId:0 → no historical backfill; the coord gates
      // replay on sinceEventId>0, and backfill's 1000-row oldest-first window
      // is the wrong tool for a specific session on a mature log). For raw
      // lifecycle history use the `sessions` projection or read the events table.
      const sid = requireArg(rest[0], "sessionId");
      const secs = numFlag(rest, "--secs", 5);
      const SKIP = new Set(["bytes", "cellGrid"]); // high-volume binary — use `cells` or `events`
      try {
        for await (const frame of await openSyncWs({ signal: AbortSignal.timeout(secs * 1000) })) {
          const fc = frame.frame.case;
          if (!fc || SKIP.has(fc)) continue;
          const val = frame.frame.value as Record<string, unknown>;
          if (fc === "sessionEvent") {
            const ev = protoToEvent(val as never) as (Record<string, unknown> & { kind: string; session_id: string; _event_id?: number }) | null;
            if (!ev || ev.session_id !== sid) continue;
            const { kind, session_id: _s, _event_id, ...body } = ev;
            console.log(`#${_event_id ?? "?"}\tsessionEvent/${kind}\t${JSON.stringify(body, jsonReplacer)}`);
            continue;
          }
          // Other frame types have no uniform session-id field; a UUID match on
          // the serialized value is precise enough (session ids are unique v4s).
          const j = JSON.stringify(val, jsonReplacer);
          if (!j || !j.includes(sid)) continue;
          console.log(`\t${fc}\t${j.length > 300 ? j.slice(0, 300) + "…" : j}`);
        }
      } catch (e) {
        // connect aborts the stream on AbortSignal.timeout — normal terminator.
        if (!/abort|timed?.?out|deadline|cancel/i.test(String(e))) throw e;
      }
      break;
    }
    case "watch": {
      // cell-phase-4: getScrollbackSince RPC retired — cell frames are the sole
      // output path. Use `events` for live output stream.
      console.error("watch: removed in cell-phase-4 — use `events` for live output stream");
      process.exit(1);
      break;
    }
    case "rename": {
      // SessionsRename: empty/omitted title CLEARS the override → auto title.
      const sid = requireArg(rest[0], "sessionId");
      const title = rest.slice(1).filter((a) => !a.startsWith("--")).join(" ");
      const r = await c.sessionsRename({ sessionId: sid, title });
      console.log(String(r.ok));
      break;
    }
    case "assign": {
      // SessionsAssignWorkspace; literal "--" clears (workspace_id absent).
      const sid = requireArg(rest[0], "sessionId");
      const ws = rest[1] === "--" ? undefined : requireArg(rest[1], "workspaceId|--");
      const r = await c.sessionsAssignWorkspace({ sessionId: sid, workspaceId: ws });
      console.log(String(r.ok));
      break;
    }
    case "message": {
      // SessionsUserMessage — the "type into the agent composer" path, NOT raw
      // PTY bytes (that's `input`). Joins remaining args so quoting is optional.
      const sid = requireArg(rest[0], "sessionId");
      requireArg(rest[1], "text");
      const r = await c.sessionsUserMessage({ sessionId: sid, text: rest.slice(1).join(" ") });
      console.log(String(r.accepted));
      break;
    }
    case "cells": {
      // Structured scrollback rows (SessionsGetScrollbackCells). end_row is
      // EXCLUSIVE and the worker clamps it to the scrollback total
      // (browser-command-terminal.ts:53), so the MAX_SAFE_INTEGER default
      // means "the newest rows". SCROLLBACK only — no live viewport; use
      // `cat` for the full grid.
      const sid = requireArg(rest[0], "sessionId");
      const maxRows = numFlag(rest, "--rows", 40);
      const end = numFlag(rest, "--end", Number.MAX_SAFE_INTEGER);
      const r = await c.sessionsGetScrollbackCells({ sessionId: sid, endRow: BigInt(end), maxRows });
      // range context → stderr; stdout stays pure row text for diffing
      console.error(`rows ${r.startRow}..${r.endRow} of ${r.scrollbackTotal} (cols ${r.cols})`);
      for (const row of r.rows) console.log(row.spans.map((s) => s.text).join("").replace(/\s+$/, ""));
      break;
    }
    case "ws-create": {
      const workerFp = requireArg(rest[0], "workerFp");
      const name = requireArg(rest[1], "name");
      const folderPath = requireArg(rest[2], "folderPath");
      const r = await c.workspacesCreate({ workerFp, name, folderPath, color: strFlag(rest, "--color") });
      console.log(r.workspace?.id ?? ""); // stdout = the new id, nothing else
      break;
    }
    case "ws-update": {
      const id = requireArg(rest[0], "id");
      const name = strFlag(rest, "--name");
      const color = strFlag(rest, "--color");
      const posRaw = strFlag(rest, "--position");
      const position = posRaw === undefined ? undefined : Number(posRaw);
      if (position !== undefined && (!Number.isInteger(position) || position < 0)) {
        console.error(`roost api: --position must be a non-negative integer, got "${posRaw}"`);
        process.exit(1);
      }
      if (name === undefined && color === undefined && position === undefined) {
        console.error("roost api: ws-update needs at least one of --name/--color/--position");
        process.exit(1);
      }
      const r = await withWsCas(c, id, (ifVersion) => c.workspacesUpdate({ id, ifVersion, name, color, position }));
      console.log(String(r.workspace?.version ?? ""));
      break;
    }
    case "ws-delete": {
      const id = requireArg(rest[0], "id");
      const r = await withWsCas(c, id, (ifVersion) => c.workspacesDelete({ id, ifVersion }));
      console.log(String(r.ok));
      break;
    }
    case "ws-set-sessions": {
      const id = requireArg(rest[0], "id");
      requireArg(rest[1], "sessionId"); // ≥1 sid — an accidental [] would GC-orphan the workspace
      const sessionIds = rest.slice(1).filter((a) => !a.startsWith("--"));
      const r = await withWsCas(c, id, (ifVersion) => c.workspacesSetSessions({ id, ifVersion, sessionIds }));
      console.log(String(r.workspace?.version ?? ""));
      break;
    }
    case "tasks": {
      const { tasks } = await c.tasksList({ state: strFlag(rest, "--state") });
      for (const t of tasks) {
        const payload = t.payloadJson.replace(/\s+/g, " ");
        console.log([
          t.id, t.state, new Date(Number(t.enqueuedAtMs)).toISOString(), t.claimedBy || "-",
          payload.length > 200 ? `${payload.slice(0, 200)}…` : payload,
        ].join("\t"));
      }
      break;
    }
    case "task-enqueue": {
      const payloadJson = requireArg(rest[0], "payload_json");
      JSON.parse(payloadJson); // fail fast here, not as an opaque queue row
      const r = await c.tasksEnqueue({ payloadJson });
      console.log(r.task?.id ?? "");
      break;
    }
    case "task-cancel": {
      const r = await c.tasksCancel({ id: requireArg(rest[0], "id") });
      console.log(r.task?.state ?? "");
      break;
    }
    case "ui-state": {
      // Spatial-model visibility: what each connected browser tab reported
      // via UiReportState (coord keeps an in-memory TTL map). Empty output =
      // no browser open → ui commands would no-op (delivered=0).
      const { tabs } = await c.uiListStates({});
      if (rest.includes("--json")) { console.log(JSON.stringify(tabs, jsonReplacer, 2)); break; }
      const now = Date.now();
      for (const t of tabs) {
        const s = t.state;
        console.log([
          t.fp.slice(0, 8), t.tabId, t.label || "-", humanAge(now - Number(t.lastMs)),
          s?.activePath ?? "", s?.focusedPaneId ?? "", (s?.visibleSessionIds ?? []).join(","),
        ].join("\t"));
        if (s?.layoutJson) printLayoutTree(s.layoutJson, s.focusedPaneId);
      }
      break;
    }
    case "ui": {
      // Drive the live SPA's pane tiling: UiDispatch → ui_command frame → the
      // browser maps it onto store/paneLayout.ts pure ops. Fire-and-forget:
      // `delivered` is the Sync subscriber count at publish, NOT per-tab acks.
      const sub = rest[0];
      const targetTabId = strFlag(rest, "--tab") ?? "";
      const pos = rest.slice(1).filter((a) => !a.startsWith("--"));
      const done = (r: { delivered: number }): void => {
        console.log(`delivered=${r.delivered}`);
        if (r.delivered === 0) console.error("roost api: delivered=0 — no browser tab connected to coord; spatial commands need a live SPA (check `roost api ui-state`)");
      };
      switch (sub) {
        case "navigate":
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "navigate", value: { path: requireArg(pos[0], "path") } } } }));
          break;
        case "place-split": {
          const dir = requireArg(pos[2], "row|col");
          if (dir !== "row" && dir !== "col") { console.error(`roost api: dir must be row|col, got "${dir}"`); process.exit(1); }
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "placeSplit", value: {
            sessionId: requireArg(pos[0], "sessionId"),
            anchorSessionId: requireArg(pos[1], "anchorSessionId"),
            dir, insertFirst: rest.includes("--first"),
          } } } }));
          break;
        }
        case "select-tab":
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "selectTab", value: { sessionId: requireArg(pos[0], "sessionId") } } } }));
          break;
        case "focus-pane":
          // focuses the pane CONTAINING that session (sync.proto UiFocusPane)
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "focusPane", value: { sessionId: requireArg(pos[0], "sessionId") } } } }));
          break;
        case "move-tab":
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "moveTab", value: { sessionId: requireArg(pos[0], "sessionId"), destSessionId: requireArg(pos[1], "destSessionId") } } } }));
          break;
        case "arrange": {
          const preset = requireArg(pos[0], "preset");
          if (!["even", "rows", "tiled", "main-vertical", "balance"].includes(preset)) {
            console.error(`roost api: preset must be even|rows|tiled|main-vertical|balance, got "${preset}"`);
            process.exit(1);
          }
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "arrange", value: { preset } } } }));
          break;
        }
        case "close-tab":
          // soft-close: SPA honors the pendingClose undo window (doClose path)
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "closeTab", value: { sessionId: requireArg(pos[0], "sessionId") } } } }));
          break;
        case "spotlight":
          done(await c.uiDispatch({ targetTabId, command: { command: { case: "spotlight", value: { sessionId: requireArg(pos[0], "sessionId"), off: rest.includes("--off") } } } }));
          break;
        default:
          console.error(
            "roost api ui <cmd> [--tab <tabId>]: navigate <path> | place-split <sid> <anchorSid> <row|col> [--first] | " +
            "select-tab <sid> | focus-pane <sid> | move-tab <sid> <destSid> | " +
            "arrange <even|rows|tiled|main-vertical|balance> | close-tab <sid> | spotlight <sid> [--off]",
          );
          process.exit(1);
      }
      break;
    }
    default:
      console.error(`roost api: unknown verb "${verb}" — sessions | cat | cells | input | message | rename | assign | attach | spawn | kill | workers | worker-rename | worker-rm | workspaces | ws-create | ws-update | ws-delete | ws-set-sessions | tasks | task-enqueue | task-cancel | ui | ui-state | agent | events | watch`);
      process.exit(1);
  }
}


/** Coarse human age for ui-state rows: "42s" / "3m" / "2h". */
function humanAge(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  return `${Math.round(s / 3600)}h`;
}

/** Indent-nested pane-tree summary parsed from UiReportState.layout_json —
 *  `split row 0.50` / `leaf [sid*,sid2]`, `*` = selectedTab. The JSON is a
 *  foreign, versionless blob (the SPA's store/paneLayout.ts Layout tree,
 *  stringified as-is so the proto doesn't chase the layout model) — walk it
 *  with runtime guards; anything off-shape prints nothing (raw via --json). */
function printLayoutTree(layoutJson: string, focusedPaneId: string): void {
  let parsed: unknown;
  try { parsed = JSON.parse(layoutJson); } catch { return; }
  if (!parsed || typeof parsed !== "object" || !("root" in parsed)) return;
  const walk = (n: unknown, indent: string): void => {
    if (!n || typeof n !== "object" || !("kind" in n)) return;
    if (n.kind === "split" && "a" in n && "b" in n) {
      const dir = "dir" in n && typeof n.dir === "string" ? n.dir : "?";
      const ratio = "ratio" in n && typeof n.ratio === "number" ? n.ratio.toFixed(2) : "?";
      console.log(`${indent}split ${dir} ${ratio}`);
      walk(n.a, indent + "  ");
      walk(n.b, indent + "  ");
      return;
    }
    if (n.kind !== "leaf") return;
    const tabs = "tabs" in n && Array.isArray(n.tabs) ? n.tabs.filter((t): t is string => typeof t === "string") : [];
    const selected = "selectedTab" in n && typeof n.selectedTab === "string" ? n.selectedTab : "";
    const paneId = "paneId" in n && typeof n.paneId === "string" ? n.paneId : "";
    const marked = tabs.map((t) => (t === selected ? `${t}*` : t));
    console.log(`${indent}leaf${paneId === focusedPaneId ? " (focused)" : ""} [${marked.join(",")}]`);
  };
  walk(parsed.root, "  ");
}
