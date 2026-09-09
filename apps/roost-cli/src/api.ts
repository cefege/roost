// roost api — headless introspection and control over coordinator Connect RPCs.
// Owns authenticated client setup and dispatches the general API verb surface.
// Focused verb families and formatters live in sibling api-* modules.
// Called by main.ts; every request uses the enrolled CLI device identity.

import { basename } from "node:path";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";
import {
  createCoordClient,
  createUnauthenticatedCoordClient,
} from "../../worker/src/coord-client.ts";
import type { CoordClient } from "../../worker/src/coord-client.ts";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { diag } from "@roost/shared/diag";
import { DEFAULT_COORDINATOR_BIND } from "@roost/shared/config";
import { buildCliContext } from "./cli-auth.ts";
import { dispatchAgentStatusApi } from "./api-agent-status.ts";
import { dispatchAgentPromptApi } from "./api-agent-prompt.ts";
import { dispatchUiApi, prepareUiApplyLayout, type PreparedUiApplyLayout } from "./api-ui.ts";
import { openSyncWs } from "./sync-ws.ts";

export type AuthorizedApiClient = CoordClient;

/** Build the production CLI client with only ~/.roost/cli-key. */
export async function buildApiClient(
  options: { coordinatorUrl?: string } = {},
): Promise<CoordClient> {
  return (await buildCliContext(options)).client;
}
/** Historical caller name; enrollment is now the normal buildApiClient path. */
export function buildSelfAuthorizedApiClient(): Promise<CoordClient> {
  return buildApiClient();
}

/**
 * Fixture-only client builder. Production CLI paths must use buildApiClient so
 * they cannot nominate a worker key. Smoke stacks seed their own isolated key.
 */
export async function buildAuthorizedApiClient(options: {
  coordinatorUrl: string;
  keyPath: string;
  label: string;
}): Promise<AuthorizedApiClient> {
  const cfg = loadWorkerConfig({
    ROOST_COORDINATOR_URL: options.coordinatorUrl,
    ROOST_WORKER_KEY_PATH: options.keyPath,
    ROOST_WORKER_LABEL: options.label,
  });
  const key = await loadWorkerKey(options.keyPath);
  return createCoordClient({
    cfg,
    getJwt: () => mintJwt(key, "roost-coordinator"),
  });
}

/** Mint a scoped one-shot worker grant using the enrolled CLI device. */
export async function mintWorkerBootstrap(
  label: string,
  coordinatorUrl?: string,
): Promise<string> {
  const client = await buildApiClient(
    coordinatorUrl ? { coordinatorUrl } : {},
  );
  return (await client.authMintBootstrap({ kind: "worker", label })).token;
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
// connect-es's internal `$typeName` so wire/event dumps stay readable.
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

async function revokeLocalDevice(args: string[]): Promise<void> {
  const fingerprint = requireArg(args[0], "fingerprint");
  if (!args.includes("--yes")) {
    throw new Error("device-revoke-local is destructive; pass --yes");
  }
  const bind = process.env.ROOST_COORDINATOR_BIND ?? DEFAULT_COORDINATOR_BIND;
  const rawUrl = process.env.ROOST_COORD_URL
    ?? `http://127.0.0.1:${new URL(`http://${bind}`).port}`;
  const url = new URL(rawUrl);
  if (
    url.protocol !== "http:"
    || url.hostname !== "127.0.0.1"
    || url.username
    || url.password
    || url.pathname !== "/"
    || url.search
    || url.hash
    || !/^[0-9]+$/.test(url.port)
  ) {
    throw new Error("device-revoke-local requires an http://127.0.0.1:<port> coordinator URL");
  }
  const client = createUnauthenticatedCoordClient(url.origin);
  const response = await client.devicesRevoke({ fingerprint });
  console.log(String(response.ok));
}

export async function api(args: string[]): Promise<void> {
  const [verb, ...rest] = args;
  if (!verb) {
    console.error("roost api <verb>: sessions | agent-status | agent-wait | agent-prompt | agents | cat | cells | input | rename | assign | attach | spawn | kill | workers | worker-rename | worker-rm | workspaces | ws-create | ws-update | ws-delete | ws-set-sessions | tasks | task-enqueue | task-cancel | ui | ui-state | events | watch");
    process.exit(1);
  }
  if (verb === "device-revoke-local") {
    try {
      await revokeLocalDevice(rest);
      return;
    } catch (error) {
      console.error(`roost api: ${error instanceof Error ? error.message : String(error)}`);
      process.exit(1);
    }
  }

  let c: CoordClient | undefined;
  let preparedUiApplyLayout: PreparedUiApplyLayout | undefined;
  try {
    if (verb === "ui" && rest[0] === "apply-layout") {
      preparedUiApplyLayout = await prepareUiApplyLayout(rest.slice(1));
    }
    // Keep stdout clean for machine consumption (jq/grep on our output).
    // loadWorkerKey logs "worker key loaded" via the shared facade to stdout;
    // shunt console.log→stderr just while building the client, then restore.
    const realLog = console.log;
    console.log = ((...a: unknown[]) => console.error(...a)) as typeof console.log;
    try { c = await buildApiClient(); } finally { console.log = realLog; }
    await dispatch(c, verb, rest, preparedUiApplyLayout);
  } catch (e) {
    // Enrollment happens while building the client. Remote and managed fresh
    // keys therefore surface their explicit pairing guidance without a retry
    // that could execute a mutating command twice.
    console.error(`roost api: ${e instanceof Error ? e.message : String(e)}`);
    process.exit(1);
  }
}

async function dispatch(
  c: CoordClient,
  verb: string,
  rest: string[],
  preparedUiApplyLayout?: PreparedUiApplyLayout,
): Promise<void> {
  if (await dispatchAgentPromptApi(c, verb, rest)) return;
  if (await dispatchAgentStatusApi(c, verb, rest)) return;
  if (await dispatchUiApi(c, verb, rest, {}, preparedUiApplyLayout)) return;
  switch (verb) {
    case "sessions": {
      const { sessions } = await c.sessionsList({ status: "all" });
      for (const s of sessions) {
        const title = s.customTitle || "";
        console.log([s.id, s.workerFp, s.kind, s.cwd, title].join("\t"));
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
      // AttachFileChunk RPC and print each abs_path (one per line). Reference
      // copy of the chunk loop: apps/web/src/lib/attachments.ts:44-78
      // (uploadId/seq/last/0-byte semantics). No shared extraction — the web
      // copy carries SPA-only concerns (serial queue, progress store) and two
      // ~15-line loops don't justify it.
      // Usage: roost api attach <sessionId> <file...> [--short-path]
      const sid = requireArg(rest[0], "sessionId");
      const unknownOption = rest.find((a) => a.startsWith("--") && a !== "--short-path");
      if (unknownOption) throw new Error(`attach: unknown option ${unknownOption}`);
      const paths = rest.slice(1).filter((a) => !a.startsWith("--"));
      requireArg(paths[0], "file");
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
      }
      break;
    }
    case "spawn": {
      const workerFp = requireArg(rest[0], "workerFp");
      const folder = requireArg(rest[1], "folder");
      const r = await c.sessionsSpawn({ workerFp, kind: "shell", folder });
      console.log(JSON.stringify({ sessionId: r.sessionId, channelId: r.channelId }));
      break;
    }
    case "kill": {
      const sid = requireArg(rest[0], "sessionId");
      const r = await c.sessionsKill({ sessionId: sid });
      console.log(String(r.accepted));
      break;
    }
    case "events": {
      // LIVE wire-delta monitor for one session over a --secs window. Prints
      // every non-binary Sync frame referencing the session, including
      // lifecycle, terminal title, and last-activity deltas.
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
        for await (const frame of await openSyncWs({
          signal: AbortSignal.timeout(secs * 1000),
        })) {
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
    default:
      console.error(`roost api: unknown verb "${verb}" — sessions | agent-status | agent-wait | agent-prompt | agents | cat | cells | input | rename | assign | attach | spawn | kill | workers | worker-rename | worker-rm | workspaces | ws-create | ws-update | ws-delete | ws-set-sessions | tasks | task-enqueue | task-cancel | ui | ui-state | events | watch`);
      process.exit(1);
  }
}

