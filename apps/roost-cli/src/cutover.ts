// `roost cutover` — R4.5 DB migration. Reads legacy coordinator.db,
// writes new coordinator_v2.db via the v2 Kysely schema.
//
// Defensive design (legacy and v2 schemas have diverged): each table
// has an explicit column-mapping. Unmapped tables get a warning and
// stay empty in v2. authorized_keys carries verbatim (identical
// schema) so existing browser JWT pins keep working.
//
// Open sessions in legacy are dropped — v2 is event-sourced; workers
// re-emit a `snapshot` on coord reconnect so live sessions reconcile
// without DB carry-over.
//
// macOS-only by construction: the legacy Rust coordinator never ran on
// Linux, so there is no v1 state on any Linux box to migrate.

import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";

const LEGACY = `${process.env.HOME}/Library/Application Support/RoostCoordinator/coordinator.db`;
const V2 = `${process.env.HOME}/Library/Application Support/RoostCoordinator/coordinator_v2.db`;

export async function cutover(args: string[]): Promise<void> {
  switch (process.platform) {
    case "darwin":
      break;
    case "linux":
    case "win32":
      console.log(`cutover is macOS-only — no legacy v1 coordinator state exists on ${process.platform}`);
      return;
    default:
      throw new Error(`unsupported cutover platform: ${process.platform}`);
  }
  const force = args.includes("--force");
  if (!existsSync(LEGACY)) {
    console.error(`legacy DB not found at ${LEGACY}`);
    process.exit(2);
  }
  if (existsSync(V2) && !force) {
    console.error(`${V2} already exists. Pass --force to overwrite.`);
    process.exit(3);
  }

  console.log(`>> reading legacy ${LEGACY}`);
  const legacy = new Database(LEGACY, { readonly: true });

  console.log(`>> writing v2 ${V2}`);
  console.log(">> running coord migrations against v2 db");
  const { runMigrations } = await import(
    new URL("../../coord/src/db/migrate.ts", import.meta.url).href
  );
  const v2 = new Database(V2);
  await runMigrations(v2);

  // ─── authorized_keys (verbatim) ─────────────────────────────────────
  {
    const rows = legacy.query(
      "SELECT fingerprint, public_key, label, added_at FROM authorized_keys",
    ).all() as { fingerprint: string; public_key: Uint8Array; label: string; added_at: number }[];
    const stmt = v2.prepare(
      "INSERT OR REPLACE INTO authorized_keys (fingerprint, public_key, label, added_at) VALUES (?, ?, ?, ?)",
    );
    for (const r of rows) stmt.run(r.fingerprint, r.public_key, r.label, r.added_at);
    console.log(`>> authorized_keys: ${rows.length} rows`);
  }

  // ─── workers (column translation) ──────────────────────────────────
  // legacy: fingerprint, label, reachable_addr, ssh_port, ssh_host_pubkey_fp,
  //         os, registered_at_ms, last_seen_ms, ws_listen_addr (string
  //         "0.0.0.0:2223" form), ws_scheme, git_sha
  // v2:     fp, label, reachable_addr, ssh_port, ws_listen_port (int),
  //         ws_scheme, os, git_sha, host_metrics_json, registered_at_ms,
  //         last_seen_ms
  {
    const rows = legacy.query(
      "SELECT fingerprint, label, reachable_addr, ssh_port, os, registered_at_ms, last_seen_ms, ws_listen_addr, ws_scheme, git_sha FROM workers",
    ).all() as {
      fingerprint: string; label: string; reachable_addr: string;
      ssh_port: number; os: string; registered_at_ms: number; last_seen_ms: number;
      ws_listen_addr: string | null; ws_scheme: string; git_sha: string | null;
    }[];
    const stmt = v2.prepare(
      "INSERT OR REPLACE INTO workers (fp, label, reachable_addr, ssh_port, ws_listen_port, ws_scheme, os, git_sha, host_metrics_json, registered_at_ms, last_seen_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)",
    );
    let count = 0;
    for (const r of rows) {
      // Parse legacy ws_listen_addr ("host:port" or "0.0.0.0:2223") → just port int.
      // Default 2223 (legacy) → 2224 (v2) is the OPERATOR'S choice; we preserve legacy
      // port here so an already-deployed legacy worker keeps connecting.
      let ws_listen_port = 2224;
      if (r.ws_listen_addr) {
        const parts = r.ws_listen_addr.split(":");
        const portStr = parts[parts.length - 1];
        const parsed = portStr ? parseInt(portStr, 10) : NaN;
        if (Number.isFinite(parsed)) ws_listen_port = parsed;
      }
      const ws_scheme = r.ws_scheme === "wss" ? "wss" : "ws";
      let os: "darwin" | "linux" | "win32";
      switch (r.os) {
        case "darwin":
        case "linux":
        case "win32":
          os = r.os;
          break;
        default:
          throw new Error(`unsupported legacy worker platform: ${r.os}`);
      }
      stmt.run(
        r.fingerprint, r.label, r.reachable_addr, r.ssh_port,
        ws_listen_port, ws_scheme, os, r.git_sha,
        r.registered_at_ms, r.last_seen_ms,
      );
      count++;
    }
    console.log(`>> workers: ${count} rows`);
  }

  // ─── workspaces (verbatim subset) ──────────────────────────────────
  // Legacy + v2 workspaces are close enough; v2 adds session_ids as a
  // junction table.
  {
    const wsCols = legacy.query("PRAGMA table_info(workspaces)").all() as { name: string }[];
    const wsNames = new Set(wsCols.map((c) => c.name));
    if (wsNames.has("id") && wsNames.has("name")) {
      const rows = legacy.query("SELECT * FROM workspaces").all() as Record<string, unknown>[];
      const stmt = v2.prepare(
        `INSERT OR REPLACE INTO workspaces (id, worker_fp, name, color, position, version, created_at_ms, updated_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      );
      let count = 0;
      for (const r of rows) {
        const id = r.id as string;
        const worker_fp = (r.worker_fingerprint ?? r.worker_fp) as string;
        const name = r.name as string;
        const color = (r.color ?? null) as string | null;
        const position = Number(r.position ?? 0);
        const version = Number(r.version ?? 0);
        const created_at_ms = Number(r.created_at_ms ?? Date.now());
        const updated_at_ms = Number(r.updated_at_ms ?? created_at_ms);
        if (!id || !worker_fp || !name) continue;
        stmt.run(id, worker_fp, name, color, position, version, created_at_ms, updated_at_ms);
        count++;
      }
      console.log(`>> workspaces: ${count} rows`);
    } else {
      console.log(">> workspaces: legacy schema missing key columns, skipped");
    }
  }

  // ─── authorized_keys-pinned tables (skip — drift too large) ───────
  // bootstrap_tokens (different column shapes), pair_requests, tasks,
  // mcp_relays, and feature_flags all diverged enough that a fresh start
  // is safer. Workers re-pair via bootstrap tokens; remaining settings
  // are operator-recreated.
  console.log(">> bootstrap_tokens / pair_requests / tasks / mcp_relays / feature_flags: skipped (operator recreates after cutover)");

  // Open sessions: NOT carried. Workers emit `snapshot` events on
  // coord reconnect (R3.1) which reconciles live sessions through the
  // event log. R4.5 spec.
  console.log(">> sessions: skipped (worker snapshot reconciles on reconnect)");

  v2.close();
  legacy.close();
  console.log(">> cutover complete");
  console.log("");
  console.log("Next steps (operator):");
  console.log("  1. Stop legacy LaunchAgents:");
  console.log("     launchctl bootout gui/$UID/com.roost.coordinator");
  console.log("     launchctl bootout gui/$UID/com.roost.worker");
  console.log("  2. Install v2 LaunchAgents:");
  console.log("     bash apps/coord/scripts/install.sh install");
  console.log("     bash apps/worker/scripts/install.sh install");
  console.log("  3. Verify browsers (existing JWT pins remain valid).");
}
