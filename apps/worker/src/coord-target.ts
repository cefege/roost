import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { Database } from "bun:sqlite";

export interface CoordTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
}

interface InflightSnapshot {
  requestId: string;
  handoffId: string;
  file: string;
  fd: number;
  expectedSize: number;
  expectedSha256: string;
  nextSeq: number;
  received: number;
  hasher: Bun.CryptoHasher;
}

interface PreparedTarget {
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
  expectedCoordKid: string;
  expectedGitSha: string;
}

export class CoordTarget {
  #inflight: InflightSnapshot | null = null;
  #prepared: PreparedTarget | null = null;

  constructor(private readonly paths: CoordTargetPaths) {}

  async prepare(request: {
    handoff_id: string; source_url: string; target_url: string; expected_coord_kid: string;
    action: "CHECK" | "PREPARE"; estimated_db_size: bigint; expected_git_sha: string;
  }): Promise<void> {
    if (process.platform !== "darwin") throw new Error("Automatic coordinator moves currently require macOS.");
    if (process.env.ROOST_GIT_SHA && process.env.ROOST_GIT_SHA !== request.expected_git_sha) {
      throw new Error(`worker version ${process.env.ROOST_GIT_SHA} does not match coordinator`);
    }
    const stat = fs.statfsSync(dirname(this.paths.dbPath));
    const available = Number(stat.bavail) * Number(stat.bsize);
    const required = Number(request.estimated_db_size) * 2 + 256 * 1024 * 1024;
    if (available < required) throw new Error(`insufficient disk: required ${required}, available ${available}`);
    if (request.action === "PREPARE") {
      fs.mkdirSync(dirname(this.paths.dbPath), { recursive: true, mode: 0o700 });
      this.#prepared = {
        handoffId: request.handoff_id, sourceUrl: request.source_url, targetUrl: request.target_url,
        expectedCoordKid: request.expected_coord_kid, expectedGitSha: request.expected_git_sha,
      };
    }
  }

  startSnapshot(request: {
    request_id: string; handoff_id: string; total_size: bigint; sha256: string; coord_key_pem: Uint8Array;
    authorized_keys: Uint8Array; secret_sha256: string; expected_worker_fps: string[];
  }): void {
    if (this.#inflight || !this.#prepared || this.#prepared.handoffId !== request.handoff_id) throw new Error("coordinator target was not prepared");
    const dir = join(this.paths.dataDir, "handoffs", request.handoff_id);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const file = join(dir, "coordinator_v2.snapshot");
    fs.writeFileSync(join(dir, "ssh_ed25519.key"), request.coord_key_pem, { mode: 0o600 });
    fs.writeFileSync(join(dir, "authorized_keys.roost"), request.authorized_keys, { mode: 0o600 });
    fs.writeFileSync(join(dir, "target-handoff.json"), JSON.stringify({
      version: 1, handoff_id: request.handoff_id, role: "TARGET", phase: "WAITING_FOR_WORKERS",
      source_url: this.#prepared.sourceUrl, target_url: this.#prepared.targetUrl, target_worker_fp: "target-pending",
      expected_worker_fps: request.expected_worker_fps, commit_acked_worker_fps: [],
      expected_coord_kid: this.#prepared.expectedCoordKid, expected_git_sha: this.#prepared.expectedGitSha,
      secret_sha256: request.secret_sha256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
    }), { mode: 0o600 });
    this.#inflight = {
      requestId: request.request_id, handoffId: request.handoff_id, file,
      fd: fs.openSync(file, "wx", 0o600), expectedSize: Number(request.total_size), expectedSha256: request.sha256,
      nextSeq: 0, received: 0, hasher: new Bun.CryptoHasher("sha256"),
    };
  }

  async appendSnapshot(chunk: { handoff_id: string; seq: number; data: Uint8Array; last: boolean }): Promise<void> {
    const inflight = this.#inflight;
    if (!inflight || inflight.handoffId !== chunk.handoff_id) throw new Error("unknown coordinator snapshot");
    if (chunk.seq !== inflight.nextSeq) throw new Error(`snapshot chunk out of order: expected ${inflight.nextSeq}, got ${chunk.seq}`);
    fs.writeSync(inflight.fd, chunk.data);
    inflight.hasher.update(chunk.data);
    inflight.received += chunk.data.length;
    inflight.nextSeq++;
    if (!chunk.last) return;
    fs.fsyncSync(inflight.fd);
    fs.closeSync(inflight.fd);
    this.#inflight = null;
    const actualSha256 = inflight.hasher.digest("hex");
    if (inflight.received !== inflight.expectedSize || actualSha256 !== inflight.expectedSha256) {
      fs.rmSync(dirname(inflight.file), { recursive: true, force: true });
      throw new Error("coordinator snapshot checksum mismatch");
    }
    const dir = dirname(inflight.file);
    const check = new Database(inflight.file, { readonly: true });
    try {
      const integrity = check.query("PRAGMA integrity_check").get() as { integrity_check?: string } | null;
      if (integrity?.integrity_check !== "ok") throw new Error("coordinator snapshot integrity check failed");
    } finally {
      check.close();
    }
    const rollbackDir = join(dir, "rollback");
    fs.mkdirSync(rollbackDir, { recursive: true, mode: 0o700 });
    for (const [canonical, name] of [
      [this.paths.dbPath, "coordinator_v2.db"],
      [this.paths.keyPath, "ssh_ed25519.key"],
      [this.paths.authorizedKeysPath, "authorized_keys.roost"],
      [this.paths.handoffPath, "coord-handoff.json"],
    ] as const) {
      if (fs.existsSync(canonical)) fs.copyFileSync(canonical, join(rollbackDir, name));
    }
    fs.writeFileSync(join(rollbackDir, "present.json"), JSON.stringify({
      db: fs.existsSync(this.paths.dbPath),
      key: fs.existsSync(this.paths.keyPath),
      authorizedKeys: fs.existsSync(this.paths.authorizedKeysPath),
      handoff: fs.existsSync(this.paths.handoffPath),
    }), { mode: 0o600 });
    fs.renameSync(inflight.file, this.paths.dbPath);
    fs.renameSync(join(dir, "ssh_ed25519.key"), this.paths.keyPath);
    fs.renameSync(join(dir, "authorized_keys.roost"), this.paths.authorizedKeysPath);
    fs.renameSync(join(dir, "target-handoff.json"), this.paths.handoffPath);
    const script = process.env.ROOST_COORDINATOR_INSTALL_SCRIPT ?? join(process.cwd(), "apps", "coord", "scripts", "install.sh");
    const child = Bun.spawn([script, "install"], {
      env: {
        ...process.env,
        ROOST_REPO_ROOT: process.cwd(),
        ROOST_COORD_DATA_DIR: this.paths.dataDir,
        ROOST_COORDINATOR_DB: this.paths.dbPath,
        ROOST_COORDINATOR_KEY_PATH: this.paths.keyPath,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: this.paths.authorizedKeysPath,
        ROOST_COORDINATOR_HANDOFF_PATH: this.paths.handoffPath,
        ROOST_COORDINATOR_PUBLIC_URL: this.#prepared!.targetUrl,
      },
      stdout: "ignore",
      stderr: "ignore",
    });
    if (await child.exited !== 0) throw new Error("pending coordinator install failed");
  }

  abort(handoffId: string): void {
    const rollback = join(this.paths.dataDir, "handoffs", handoffId, "rollback");
    let present: { db: boolean; key: boolean; authorizedKeys: boolean; handoff: boolean };
    try { present = JSON.parse(fs.readFileSync(join(rollback, "present.json"), "utf8")); }
    catch { return; }
    for (const [canonical, name, existed] of [
      [this.paths.dbPath, "coordinator_v2.db", present.db],
      [this.paths.keyPath, "ssh_ed25519.key", present.key],
      [this.paths.authorizedKeysPath, "authorized_keys.roost", present.authorizedKeys],
      [this.paths.handoffPath, "coord-handoff.json", present.handoff],
    ] as const) {
      const backup = join(rollback, name);
      if (existed) fs.copyFileSync(backup, canonical);
      else fs.rmSync(canonical, { force: true });
    }
  }
  snapshotPath(): string | null {
    return this.#inflight?.file ?? null;
  }
}
