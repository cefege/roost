// `roost doctor [--since 24h]` — daily anomaly digest for production review.
//
// Reads the LOW-VOLUME Tier-1 channel: coord + worker `main.err.log`
// (+ rotated `.N.gz`), where every always-on `signal()` event lands
// (`target:"signal"`, via `log.warn`) alongside infra warnings
// (reconnect `reader_failed`, `pending-rpcs timeout`, etc). The high-volume
// `diag()` firehose in `*.out.log` is deliberately NOT read here.
//
// Output: one-screen markdown digest grouped by signal kind + infra
// target/msg. Exit 1 when any signal fired or any error-level line exists
// (so it is cron/CI-able: 0 = nothing to review). Health (tailscale/agents/
// coord) lives in `roost status` — this command is logs-only and fast.
//
// SPA signals all flow through coord → coord's err.log covers every device.
// Remote-worker signals stay on their host until Phase-5 centralization.

import { existsSync, readdirSync, createReadStream } from "node:fs";
import { createInterface } from "node:readline";
import { createGunzip } from "node:zlib";
import { coordLogDir, workerLogDir } from "@roost/shared/paths";

const COORD_LOGS = coordLogDir();
const WORKER_LOGS = workerLogDir();
const SOURCES: Array<{ app: string; dir: string; base: string }> = [
  { app: "coord", dir: COORD_LOGS, base: "main.err.log" },
  { app: "worker", dir: WORKER_LOGS, base: "main.err.log" },
  { app: "keeper", dir: WORKER_LOGS, base: "keeper.err.log" },
];

// Signal kinds that are genuine errors (vs warnings worth surfacing). Used
// only for the headline icon; ANY signal sets a non-zero exit regardless.
const ERROR_SIGNALS: Record<string, true> = {
  "spa.uncaught": true,
  "auth.relogin_401": true,
  "voice.ws_failed": true,
  "worker.uncaught": true,
  "transport.event_drop": true,
  "event.append_failed": true,
  "audit.write_failed": true,
  "sync.backfill_failed": true,
  "deploy.failed": true,
  "scrollback.history_lost": true,
};

interface LogLine {
  ts?: number;
  level?: string;
  target?: string;
  evt?: string;   // signal/diag kind (survives a kv `msg` collision)
  msg?: string;
  kind?: string;
  sid?: string;
  src?: string;
  [k: string]: unknown;
}

interface SignalGroup {
  count: number;
  sids: Set<string>;
  srcs: Set<string>;
  subKinds: Map<string, number>; // e.g. corruption_signal → {resize_storm: 4}
  samples: string[];             // distinct short messages
}

function parseSince(args: string[]): { ms: number; label: string } {
  let label = "24h";
  for (let i = 0; i < args.length; i++) {
    const a = args[i]!;
    if (a === "--since" && args[i + 1]) { label = args[i + 1]!; i++; }
    else if (a.startsWith("--since=")) label = a.slice("--since=".length);
  }
  const m = /^(\d+)([smhd])$/.exec(label);
  if (!m) { console.error(`bad --since "${label}" (use e.g. 24h, 7d, 90m)`); process.exit(2); }
  const n = Number(m[1]);
  const unit = { s: 1e3, m: 60e3, h: 3600e3, d: 86400e3 }[m[2]!]!;
  return { ms: n * unit, label };
}

/** All err-log files for a source, newest base first then rotated .gz. */
function logFilesFor(dir: string, base: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => f === base || f.startsWith(`${base}.`))
    .map((f) => `${dir}/${f}`);
}

/** Yield parsed JSON log objects from a file (transparently gunzips .gz). */
async function* readLogLines(file: string): AsyncGenerator<LogLine> {
  const raw = createReadStream(file);
  const stream = file.endsWith(".gz") ? raw.pipe(createGunzip()) : raw;
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  for await (const line of rl) {
    if (!line) continue;
    try { yield JSON.parse(line) as LogLine; } catch { /* skip non-JSON */ }
  }
}

function short(s: unknown, n = 70): string {
  const str = String(s ?? "");
  return str.length > n ? str.slice(0, n - 1) + "…" : str;
}

export interface Digest {
  signals: Map<string, SignalGroup>;
  infra: Map<string, number>;
  errorLines: number;
  minTs: number;
  maxTs: number;
}

export function newDigest(): Digest {
  return { signals: new Map(), infra: new Map(), errorLines: 0, minTs: Infinity, maxTs: -Infinity };
}

/** Fold one log line into the digest. Filters by cutoff + warn/error level;
 *  splits `target:"signal"` (Tier-1 anomalies) from infra warnings. Pure +
 *  side-effects-on-acc so it is unit-testable with synthetic fixtures. */
export function classify(d: Digest, e: LogLine, cutoff: number, srcApp: string): void {
  const ts = typeof e.ts === "number" ? e.ts : 0;
  if (ts < cutoff) return;
  const level = e.level ?? "";
  if (level !== "warn" && level !== "error") return;
  if (ts < d.minTs) d.minTs = ts;
  if (ts > d.maxTs) d.maxTs = ts;
  if (level === "error") d.errorLines++;

  if (e.target === "signal") {
    // Group by evt (the kind); fall back to msg for Bun-side signals or
    // pre-evt log lines. A kv `msg` (e.g. error text) never wins here.
    const kind = e.evt ?? e.msg ?? "unknown";
    let g = d.signals.get(kind);
    if (!g) { g = { count: 0, sids: new Set(), srcs: new Set(), subKinds: new Map(), samples: [] }; d.signals.set(kind, g); }
    g.count++;
    if (e.sid) g.sids.add(String(e.sid));
    g.srcs.add(String(e.src ?? srcApp));
    if (e.kind) g.subKinds.set(String(e.kind), (g.subKinds.get(String(e.kind)) ?? 0) + 1);
  } else {
    const key = `${e.target ?? "?"} / ${short(e.msg, 48)}`;
    d.infra.set(key, (d.infra.get(key) ?? 0) + 1);
  }
}

export function renderDigest(d: Digest, sinceLabel: string, cutoff: number, missing: string[]): { text: string; exit: number } {
  const fmt = (ts: number) => new Date(ts).toISOString().replace("T", " ").slice(0, 16);
  const out: string[] = [];
  out.push(`# roost doctor — last ${sinceLabel}`);
  out.push(`host=${process.env.HOST ?? "local"}  sources=coord+worker(local)  cutoff=${fmt(cutoff)}`);
  out.push("");

  const totalSignals = [...d.signals.values()].reduce((a, g) => a + g.count, 0);
  out.push("## signals (always-on anomaly channel)");
  if (d.signals.size === 0) {
    out.push("  ✓ none in window");
  } else {
    for (const [kind, g] of [...d.signals.entries()].sort((a, b) => b[1].count - a[1].count)) {
      const icon = ERROR_SIGNALS[kind] ? "🔴" : "⚠️ ";
      const sub = g.subKinds.size
        ? "  " + [...g.subKinds.entries()].sort((a, b) => b[1] - a[1]).map(([k, n]) => `${k}×${n}`).join(" ")
        : "";
      const scope = `  (${g.sids.size ? `${g.sids.size} sid${g.sids.size > 1 ? "s" : ""}, ` : ""}src:${[...g.srcs].join("/")})`;
      out.push(`  ${icon} ${kind.padEnd(24)} ${String(g.count).padStart(3)}${sub}${scope}`);
    }
  }
  out.push("");

  out.push("## infra warnings (reconnect / rate-limit / external)");
  if (d.infra.size === 0) {
    out.push("  ✓ none in window");
  } else {
    for (const [key, n] of [...d.infra.entries()].sort((a, b) => b[1] - a[1]).slice(0, 15)) {
      out.push(`  ${String(n).padStart(4)}  ${key}`);
    }
  }
  out.push("");

  if (missing.length) out.push(`note: no err.log for ${missing.join(", ")} (service never ran on this host?)`);
  out.push("## summary");
  out.push(`  window:  ${d.minTs === Infinity ? "(no events)" : `${fmt(d.minTs)} → ${fmt(d.maxTs)}`}`);
  out.push(`  signals: ${totalSignals} (${d.signals.size} kinds)   infra: ${[...d.infra.values()].reduce((a, b) => a + b, 0)}   errors: ${d.errorLines}`);
  out.push(`  health:  run \`roost status\` (tailscale / launch agents / coord / workers)`);

  const exit = totalSignals > 0 || d.errorLines > 0 ? 1 : 0;
  out.push(`  exit:    ${exit} (${exit ? "review above" : "nothing to review"})`);
  return { text: out.join("\n"), exit };
}

/** Does this log line belong to session `sid` (full id or prefix)? */
function matchesSid(e: LogLine, sid: string): boolean {
  const cand = [e.sid, (e as Record<string, unknown>).session_id, (e as Record<string, unknown>).sessionId];
  return cand.some((v) => typeof v === "string" && (v === sid || v.startsWith(sid)));
}

/** `roost doctor --session <sid>` — chronological event timeline for ONE
 *  session, merged across coord+worker err.log (signals) AND out.log (the
 *  diag firehose: bytes/cell/focus/session events). Answers "is this session
 *  alive / when did it last echo / where did input stop" without log
 *  spelunking. Needs ROOST_DIAG=1 for the firehose half (signals show either way). */
async function sessionTimeline(sid: string, sinceMs: number): Promise<void> {
  const cutoff = Date.now() - sinceMs;
  const SKIP = new Set(["ts", "level", "target", "evt", "msg", "sid", "session_id", "sessionId", "mono_ns", "session_trace_id"]);
  const rows: Array<{ ts: number; mono: number; app: string; chan: string; label: string }> = [];
  for (const src of SOURCES) {
    for (const base of ["main.err.log", "main.out.log"]) {
      for (const file of logFilesFor(src.dir, base)) {
        for await (const e of readLogLines(file)) {
          const ts = typeof e.ts === "number" ? e.ts : 0;
          if (ts < cutoff || !matchesSid(e, sid)) continue;
          const evt = e.evt ?? e.msg ?? "?";
          const chan = e.target === "signal" ? "SIGNAL" : (e.target ?? e.level ?? "log");
          const kv = Object.entries(e).filter(([k]) => !SKIP.has(k))
            .slice(0, 6).map(([k, v]) => `${k}=${short(v, 24)}`).join(" ");
          rows.push({ ts, mono: typeof e.mono_ns === "number" ? e.mono_ns : 0, app: src.app, chan, label: `${evt} ${kv}`.trim() });
        }
      }
    }
  }
  rows.sort((a, b) => a.ts - b.ts || a.mono - b.mono);
  const hhmmss = (ts: number) => new Date(ts).toISOString().replace("T", " ").slice(11, 23);
  console.log(`# roost doctor --session ${sid}  (${rows.length} events)`);
  if (!rows.length) {
    console.log("  no events. The diag firehose is gated — set ROOST_DIAG=1 (worker/coord) +");
    console.log("  localStorage.roostDiag='1' (SPA) and reproduce; signals show without it.");
    process.exit(0);
  }
  for (const r of rows) console.log(`  ${hhmmss(r.ts)} ${r.app.padEnd(6)} ${r.chan.padEnd(7)} ${r.label}`);
  process.exit(0);
}

export async function doctor(args: string[]): Promise<void> {
  const since = parseSince(args);
  const sidArg = args.find((a) => a.startsWith("--session="))?.slice("--session=".length)
    ?? (args.includes("--session") ? args[args.indexOf("--session") + 1] : undefined);
  if (sidArg) { await sessionTimeline(sidArg, since.ms); return; }
  const cutoff = Date.now() - since.ms;
  const d = newDigest();
  const missing: string[] = [];

  for (const src of SOURCES) {
    const files = logFilesFor(src.dir, src.base);
    if (files.length === 0) { missing.push(`${src.app} (${src.dir})`); continue; }
    for (const file of files) {
      for await (const e of readLogLines(file)) classify(d, e, cutoff, src.app);
    }
  }

  const { text, exit } = renderDigest(d, since.label, cutoff, missing);
  console.log(text);
  process.exit(exit);
}
