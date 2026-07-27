import * as fs from "node:fs";
import { dirname } from "node:path";
import { log } from "@roost/shared/log";

export interface CoordRelocationJournal {
  version: 1;
  handoff_id: string;
  source_url: string;
  target_url: string;
  state: "STAGED" | "ACTIVATED" | "COMMITTED";
  updated_at_ms: number;
}

/**
 * Keeps the worker's runtime and LaunchAgent endpoint coherent across a move.
 * The journal is the recovery source of truth until a verified COMMIT or ABORT.
 */
export class WorkerCoordRelocation {
  constructor(private readonly path: string, private readonly workerServicePath: string) {}

  load(): CoordRelocationJournal | null {
    try {
      const parsed = JSON.parse(fs.readFileSync(this.path, "utf8")) as CoordRelocationJournal;
      return parsed.version === 1 ? parsed : null;
    } catch {
      return null;
    }
  }

  stage(request: { handoff_id: string; source_url: string; target_url: string }): void {
    this.write({ version: 1, handoff_id: request.handoff_id, source_url: request.source_url, target_url: request.target_url, state: "STAGED", updated_at_ms: Date.now() });
  }
  /** Drops a journal that can no longer influence anything. */
  discard(): void {
    fs.rmSync(this.path, { force: true });
    this.fsyncParent();
  }

  activate(request: { handoff_id: string; source_url: string; target_url: string }): void {
    const current = this.load();
    // Hijacking another move's journal would repoint this worker at a
    // coordinator nobody asked for.
    if (current && current.handoff_id !== request.handoff_id && current.state !== "COMMITTED") {
      throw new Error(`worker already has an active relocation for handoff ${current.handoff_id}`);
    }
    // No stage() first: the STAGED record it wrote was never observable and
    // cost two extra fsyncs.
    this.write({ version: 1, handoff_id: request.handoff_id, source_url: request.source_url, target_url: request.target_url, state: "ACTIVATED", updated_at_ms: Date.now() });
  }

  async commit(unackedEventCount: () => number, relocate: (url: string, force?: boolean) => void): Promise<void> {
    const current = this.requireActivated();
    const deadline = Date.now() + 30_000;
    while (unackedEventCount() > 0 && Date.now() < deadline) await Bun.sleep(25);
    const residual = unackedEventCount();
    if (residual > 0) throw new Error(`worker event drain timed out after 30000ms with ${residual} unacked events`);
    await this.persistEndpoint(current.target_url);
    // Keep the journal. launchd/systemd hold the environment they were loaded
    // with, so a `kickstart -k` / `systemctl restart` before the next full
    // login would otherwise re-exec this worker against the retired source
    // with nothing left on disk to correct it.
    this.write({ ...current, state: "COMMITTED", updated_at_ms: Date.now() });
    // handleDownstream sends rpc-ok in a microtask after this callback resolves.
    setTimeout(() => relocate(current.target_url, true), 0);
  }
  async abort(handoffId: string, relocate: (url: string) => void): Promise<void> {
    const current = this.load();
    if (!current) return;
    // Mirrors activate()'s guard: a late ABORT must not repoint a live,
    // already-committed worker back at the retired source, nor act on a
    // different handoff's journal.
    if (current.state === "COMMITTED" || current.handoff_id !== handoffId) return;
    await this.persistEndpoint(current.source_url);
    fs.rmSync(this.path, { force: true });
    this.fsyncParent();
    relocate(current.source_url);
  }

  private requireActivated(): CoordRelocationJournal {
    const current = this.load();
    if (!current || current.state !== "ACTIVATED") throw new Error("worker relocation was not activated");
    return current;
  }

  /** Rewrites ROOST_COORDINATOR_URL in the worker's own service definition,
   *  write-then-verify on both platforms. Deliberately does NOT restart the
   *  service: install.sh's migrate-env bootstraps, which would kill the very
   *  process running this commit. */
  private async persistEndpoint(url: string): Promise<void> {
    if (!fs.existsSync(this.workerServicePath)) {
      throw new Error(`worker service definition is missing at ${this.workerServicePath}`);
    }
    if (process.platform === "darwin") {
      const set = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", `Set :EnvironmentVariables:ROOST_COORDINATOR_URL ${url}`, this.workerServicePath], { stdout: "ignore", stderr: "ignore" });
      if (await set.exited !== 0) throw new Error("failed to update worker coordinator URL");
      const get = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", "Print :EnvironmentVariables:ROOST_COORDINATOR_URL", this.workerServicePath], { stdout: "pipe", stderr: "ignore" });
      const actual = (await new Response(get.stdout).text()).trim();
      if (await get.exited !== 0 || actual !== url) throw new Error("worker coordinator URL verification failed");
      return;
    }
    const original = fs.readFileSync(this.workerServicePath, "utf8");
    const line = `Environment=ROOST_COORDINATOR_URL=${url}`;
    const rewritten = /^Environment=ROOST_COORDINATOR_URL=.*$/m.test(original)
      ? original.replace(/^Environment=ROOST_COORDINATOR_URL=.*$/m, line)
      : original.replace(/^\[Service\]$/m, `[Service]\n${line}`);
    if (!rewritten.includes(line)) throw new Error("failed to update worker coordinator URL");
    const temporary = `${this.workerServicePath}.tmp-${process.pid}`;
    fs.writeFileSync(temporary, rewritten, { mode: 0o644 });
    fs.renameSync(temporary, this.workerServicePath);
    // daemon-reload re-reads unit files without restarting running units.
    const reload = Bun.spawn(["systemctl", "--user", "daemon-reload"], {
      stdout: "ignore", stderr: "ignore",
      env: { ...process.env, XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? ""}` },
    });
    if (await reload.exited !== 0) log.warn("worker", "systemd_daemon_reload_failed", { path: this.workerServicePath });
    const actual = fs.readFileSync(this.workerServicePath, "utf8");
    if (!actual.includes(line)) throw new Error("worker coordinator URL verification failed");
  }

  private write(value: CoordRelocationJournal): void {
    fs.mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    const temporary = `${this.path}.tmp-${process.pid}`;
    const fd = fs.openSync(temporary, "w", 0o600);
    try {
      fs.writeFileSync(fd, JSON.stringify(value));
      fs.fsyncSync(fd);
    } finally {
      fs.closeSync(fd);
    }
    fs.renameSync(temporary, this.path);
    this.fsyncParent();
  }

  private fsyncParent(): void {
    const fd = fs.openSync(dirname(this.path), "r");
    try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  }
}
