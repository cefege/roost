import * as fs from "node:fs";
import { dirname } from "node:path";

export interface CoordRelocationJournal {
  version: 1;
  handoff_id: string;
  source_url: string;
  target_url: string;
  state: "STAGED" | "ACTIVATED";
  updated_at_ms: number;
}

/**
 * Keeps the worker's runtime and LaunchAgent endpoint coherent across a move.
 * The journal is the recovery source of truth until a verified COMMIT or ABORT.
 */
export class WorkerCoordRelocation {
  constructor(private readonly path: string, private readonly workerPlistPath: string) {}

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

  activate(request: { handoff_id: string; source_url: string; target_url: string }): void {
    const current = this.load();
    if (!current || current.handoff_id !== request.handoff_id) this.stage(request);
    this.write({ version: 1, handoff_id: request.handoff_id, source_url: request.source_url, target_url: request.target_url, state: "ACTIVATED", updated_at_ms: Date.now() });
  }

  async commit(unackedEventCount: () => number, relocate: (url: string, force?: boolean) => void): Promise<void> {
    const current = this.requireActivated();
    const deadline = Date.now() + 30_000;
    while (unackedEventCount() > 0 && Date.now() < deadline) await Bun.sleep(25);
    if (unackedEventCount() > 0) throw new Error("worker event drain timed out");
    await this.persistEndpoint(current.target_url);
    fs.rmSync(this.path, { force: true });
    this.fsyncParent();
    // handleDownstream sends rpc-ok in a microtask after this callback resolves.
    setTimeout(() => relocate(current.target_url, true), 0);
  }

  async abort(relocate: (url: string) => void): Promise<void> {
    const current = this.load();
    if (!current) return;
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

  private async persistEndpoint(url: string): Promise<void> {
    if (!fs.existsSync(this.workerPlistPath)) throw new Error("worker LaunchAgent plist is missing");
    const set = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", `Set :EnvironmentVariables:ROOST_COORDINATOR_URL ${url}`, this.workerPlistPath], { stdout: "ignore", stderr: "ignore" });
    if (await set.exited !== 0) throw new Error("failed to update worker coordinator URL");
    const get = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", "Print :EnvironmentVariables:ROOST_COORDINATOR_URL", this.workerPlistPath], { stdout: "pipe", stderr: "ignore" });
    const actual = (await new Response(get.stdout).text()).trim();
    if (await get.exited !== 0 || actual !== url) throw new Error("worker coordinator URL verification failed");
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
