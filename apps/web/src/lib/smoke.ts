// Smoke backdoor — exposes a window.__smoke API so /roost-smoke
// (humanchrome) can drive PTY input + read state without going
// through wterm's textarea + KeyboardEvent pipeline (which is brittle
// for synthetic events).
//
// Only installed when localStorage.roostSmoke === "1". User runs
// `localStorage.roostSmoke = "1"; location.reload()` in the smoke tab.
//
// Phase-26 smoke-backdoor. crpc6: migrated from tRPC to Connect.

import { coordClient } from "../connect.ts";
import { forceSyncReconnect as forceSyncReconnectImpl, cellFrameCount as cellFrameCountImpl } from "../store/sync.ts";
import { rootStore } from "../store/root.ts";
import { setForceVisible } from "./pageVisible.ts";

interface SmokeApi {
  /** Send raw bytes via the coord RPC — BYPASSES the wterm textarea + focus
   *  pipeline. Use for variant coverage only, NEVER as the "can input?" test:
   *  it stays green when focus is dead (the cell-phase-3b input bug). For the real
   *  input check use paneFocused() + real keystrokes (chrome_keyboard). */
  input(sessionId: string, text: string): Promise<void>;
  /** Real-input regression probe: is the active pane's textarea actually the
   *  document.activeElement? Returns false when focus never landed (= input is
   *  dead even though input() would still succeed). This is the assertion that
   *  catches "I can't input anything". */
  paneFocused(sessionId: string): { hasSlot: boolean; hasTextarea: boolean; focused: boolean };
  /** Visible text of a pane's rendered grid (cell rows or wterm rows). Used to
   *  assert a typed marker actually echoed back through the full chain. */
  viewportText(sessionId: string): string;
  /** Render-correctness probe: the REAL painted scroll geometry + row counts
   *  for a pane. This is what catches "scroll jumped to top", "history lost",
   *  "rows duplicated" — none of which a wire/data-* test can see. Reads the
   *  actual scroll container (.wterm, shared by byte + cell renderers). */
  renderProbe(sessionId: string): {
    found: boolean;
    mode: "byte" | "cell" | "none";
    scrollTop: number; scrollHeight: number; clientHeight: number;
    fromBottom: number; atBottom: boolean;
    rowCount: number; nonEmptyRows: number;
    firstLine: string; lastLine: string;
  };
  /** Scan EVERY rendered row for `${prefix}<N>` markers. Detects history depth
   *  (min/max N), loss (missing Ns in [min,max]), and CORRUPTION (any N seen
   *  more than once = duplicated rows, the cell-mode tab-switch bug). */
  markerScan(sessionId: string, prefix: string): {
    total: number; unique: number; min: number; max: number;
    duplicated: number[]; missing: number;
    // outOfOrder: count of render-order inversions (= mangled/out-of-position
    // rows). firstInversion: the marker N where order first breaks, or -1.
    outOfOrder: number; firstInversion: number;
  };
  /** Scroll a pane to an edge so the harness can assert history is reachable. */
  scrollToEdge(sessionId: string, edge: "top" | "bottom"): void;
  /** Snapshot of current SPA store state. */
  state(): {
    sessions: Record<string, unknown>;
    workspaces: Record<string, unknown>;
    workers: Record<string, unknown>;
    pair_requests: Record<string, unknown>;
  };
  /** Automation visibility pin: treat the page as foregrounded while the tab
   *  is hidden/occluded — claims stay held, timers tick, sync stream stays
   *  managed (lib/pageVisible.ts::setForceVisible). Fixes the FOREGROUND
   *  GOTCHA class of false verification failures (Author 2026-07-11 "push to
   *  front via API"). Chrome may still starve a long-backgrounded tab's
   *  HTTP/2 stream at the transport layer — keep hidden probe windows short. */
  forceVisible(on: boolean): void;
  /** Force a firehose WebSocket reconnect (closes the live WS). */
  forceSyncReconnect(): void;
  /** How many cell frames have arrived for this session (smoke verification). */
  cellFrameCount(sessionId: string): number;
  /** Kill a session via the standard mutation. */
  kill(sessionId: string): Promise<{ accepted: boolean }>;
  /** Spawn a shell on a worker; returns { session_id, channel_id }. */
  spawnShell(workerFp: string, folder: string): Promise<{ session_id: string; channel_id: number }>;
  /** Spawn a real claude (kind:"claude", alt-screen) session. */
  spawnClaude(workerFp: string, folder: string): Promise<{ session_id: string; channel_id: number }>;
  /** Create a workspace attached to a session — bypasses the cwd-picker UI. */
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  /** Session ids THIS tab spawned (cleanup allowlist). */
  _spawned: string[];
  /** Kill ONLY this tab's spawned sessions. Use for cleanup — NEVER iterate
   *  state().sessions to kill (that hits Author's live sessions, not just test
   *  ones — see feedback_never_mass_kill_live_sessions). */
  killSpawned(): Promise<{ killed: string[] }>;
  /** att1-stream e2e: upload a synthetic file of `sizeBytes` through the REAL
   *  chunked uploadAttachment path (Blob.slice → AttachFileChunk → worker). Used
   *  to verify uploads >50 MB round-trip end-to-end. Returns the worker abs_path. */
  uploadAttachment(sessionId: string, sizeBytes: number, filename?: string): Promise<{ abs_path: string }>;
  /** att3 dedup probe: ask the worker whether it already holds content with
   *  this SHA-256. Returns { hit, abs_path }. */
  attachmentProbe(sessionId: string, sha256: string, sizeBytes: number, filename?: string): Promise<{ hit: boolean; abs_path: string }>;
  /** Chunked-download e2e: pull a worker file via the filesReadChunk loop and
   *  return the reassembled byte length + hex SHA-256 (integrity + no-size-cap
   *  proof). */
  downloadWorkerFile(workerFp: string, path: string): Promise<{ bytes: number; sha256: string }>;
}

export function maybeInstallSmokeBackdoor(): void {
  if (typeof window === "undefined") return;
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;

  const spawned: string[] = [];
  const api: SmokeApi = {
    _spawned: spawned,
    async killSpawned() {
      // ONLY this tab's spawns. Guards against the 2026-06-22 mass-kill
      // (looping state().sessions killed all of Author's live sessions).
      const killed: string[] = [];
      for (const sid of spawned.splice(0)) {
        try { await coordClient.sessionsKill({ sessionId: sid }); killed.push(sid); }
        catch { /* already gone */ }
      }
      return { killed };
    },
    async input(sessionId, text) {
      await coordClient.sessionsInput({
        sessionId,
        data: new TextEncoder().encode(text),
      });
    },
    paneFocused(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const ta = slot?.querySelector("textarea") ?? null;
      return {
        hasSlot: !!slot,
        hasTextarea: !!ta,
        focused: !!ta && document.activeElement === ta,
      };
    },
    viewportText(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      return (slot?.textContent ?? "").replace(/\s+/g, " ").trim();
    },
    renderProbe(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      // .wterm is the scroll container for BOTH renderers (cell-grid also adds
      // the .wterm class). Cell rows are .cell-row; byte rows are .term-row.
      const c = slot?.querySelector(".wterm") as HTMLElement | null;
      if (!c) {
        return { found: false, mode: "none" as const, scrollTop: 0, scrollHeight: 0,
          clientHeight: 0, fromBottom: 0, atBottom: false, rowCount: 0,
          nonEmptyRows: 0, firstLine: "", lastLine: "" };
      }
      const isCell = c.classList.contains("cell-grid");
      const rows = Array.from(c.querySelectorAll(isCell ? ".cell-row" : ".term-row"))
        .map((r) => (r.textContent ?? "").replace(/ /g, " ").replace(/\s+$/, ""));
      const nonEmpty = rows.filter((t) => t.trim() !== "");
      const fromBottom = c.scrollHeight - c.scrollTop - c.clientHeight;
      return {
        found: true, mode: isCell ? "cell" as const : "byte" as const,
        scrollTop: Math.round(c.scrollTop), scrollHeight: Math.round(c.scrollHeight),
        clientHeight: Math.round(c.clientHeight), fromBottom: Math.round(fromBottom),
        atBottom: fromBottom <= 2,
        rowCount: rows.length, nonEmptyRows: nonEmpty.length,
        firstLine: nonEmpty[0] ?? "", lastLine: nonEmpty[nonEmpty.length - 1] ?? "",
      };
    },
    markerScan(sessionId, prefix) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const c = slot?.querySelector(".wterm") as HTMLElement | null;
      const text = c?.textContent ?? "";
      const re = new RegExp(prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "(\\d+)", "g");
      const counts = new Map<number, number>();
      const seq: number[] = []; // marker Ns in DOM render order
      let m: RegExpExecArray | null;
      while ((m = re.exec(text)) !== null) {
        const n = Number(m[1]);
        counts.set(n, (counts.get(n) ?? 0) + 1);
        seq.push(n);
      }
      const ns = [...counts.keys()];
      const total = [...counts.values()].reduce((a, b) => a + b, 0);
      const min = ns.length ? Math.min(...ns) : 0;
      const max = ns.length ? Math.max(...ns) : 0;
      const duplicated = ns.filter((n) => (counts.get(n) ?? 0) > 1).sort((a, b) => a - b);
      let missing = 0;
      for (let n = min; n <= max; n++) if (!counts.has(n)) missing++;
      // Render-order inversions: `seq 1..N | echo` is strictly increasing, so
      // ANY render-order drop (seq[i+1] < seq[i]) = MANGLE — rows out of
      // position. This is the secondary-device-mangle / tab-switch-corruption
      // symptom that dup/loss counts alone miss.
      let outOfOrder = 0;
      let firstInversion = -1;
      for (let i = 1; i < seq.length; i++) {
        if (seq[i]! < seq[i - 1]!) {
          outOfOrder++;
          if (firstInversion < 0) firstInversion = seq[i]!;
        }
      }
      return { total, unique: ns.length, min, max, duplicated, missing, outOfOrder, firstInversion };
    },
    scrollToEdge(sessionId, edge) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const c = slot?.querySelector(".wterm") as HTMLElement | null;
      if (!c) return;
      c.scrollTop = edge === "top" ? 0 : c.scrollHeight;
    },
    state() {
      return {
        sessions: { ...rootStore.sessions },
        workspaces: { ...rootStore.workspaces },
        workers: { ...rootStore.workers },
        pair_requests: { ...rootStore.pair_requests },
      };
    },
    forceSyncReconnect() {
      forceSyncReconnectImpl();
    },
    cellFrameCount(sessionId) {
      return cellFrameCountImpl(sessionId);
    },
    forceVisible(on) {
      setForceVisible(on);
    },
    async kill(sessionId) {
      const res = await coordClient.sessionsKill({ sessionId });
      return { accepted: res.accepted };
    },
    async spawnShell(workerFp, folder) {
      const res = await coordClient.sessionsSpawn({ workerFp, kind: "shell", folder });
      spawned.push(res.sessionId);
      return { session_id: res.sessionId, channel_id: res.channelId };
    },
    async spawnClaude(workerFp, folder) {
      // kind:"claude" → worker spawns the claude bridge (alt-screen, absolute
      // cursor). Exercises the real claude render path the harness must cover.
      const res = await coordClient.sessionsSpawn({ workerFp, kind: "claude", folder });
      spawned.push(res.sessionId);
      return { session_id: res.sessionId, channel_id: res.channelId };
    },
    async createWorkspace(workerFp, folder, sessionId) {
      const existing = new Set(
        Object.values(rootStore.workspaces)
          .filter((w: { worker_fp?: string; name?: string }) => w.worker_fp === workerFp)
          .map((w: { name?: string }) => w.name ?? "")
      );
      const base = folder.split("/").filter(Boolean).pop() ?? "~";
      let name = base;
      let i = 2;
      while (existing.has(name)) { name = `${base} ${i++}`; }
      const ws = await coordClient.workspacesCreate({
        workerFp,
        name,
        folderPath: folder,
        attachSessionIds: [sessionId],
      });
      const session = rootStore.sessions[sessionId] as { channel?: number } | undefined;
      return { id: ws.workspace!.id, channel: session?.channel ?? 0 };
    },
    async uploadAttachment(sessionId, sizeBytes, filename = `smoke-${sizeBytes}.bin`) {
      const { uploadAttachment } = await import("./attachments.ts");
      // Distinct bytes (not all-zero) so a corrupt assembly would mismatch.
      const buf = new Uint8Array(sizeBytes);
      for (let i = 0; i < sizeBytes; i++) buf[i] = i & 0xff;
      const file = new File([buf], filename, { type: "application/octet-stream" });
      return uploadAttachment({ id: sessionId }, file);
    },
    async attachmentProbe(sessionId, sha256, sizeBytes, filename = "probe.bin") {
      const res = await coordClient.attachmentProbe({
        sessionId, sha256, size: BigInt(sizeBytes), filename, shortPath: false,
      });
      return { hit: res.hit, abs_path: res.absPath };
    },
    async downloadWorkerFile(workerFp, path) {
      const CHUNK = 4 * 1024 * 1024;
      const parts: Uint8Array[] = [];
      let offset = 0;
      let bytes = 0;
      for (;;) {
        const res = await coordClient.filesReadChunk({ workerFp, path, offset: BigInt(offset), len: CHUNK });
        if (res.data.length) { parts.push(res.data); offset += res.data.length; bytes += res.data.length; }
        if (res.eof || res.data.length === 0) break;
      }
      const all = new Uint8Array(bytes);
      let p = 0;
      for (const part of parts) { all.set(part, p); p += part.length; }
      const digest = await crypto.subtle.digest("SHA-256", all);
      const sha256 = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
      return { bytes, sha256 };
    },
  };
  (window as Window & { __smoke?: SmokeApi }).__smoke = api;
  console.debug("[smoke] backdoor installed via window.__smoke");
}
