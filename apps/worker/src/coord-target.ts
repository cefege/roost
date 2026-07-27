import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { createHash } from "node:crypto";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { Database } from "bun:sqlite";
import { COORD_INSTALL_SH } from "@roost/shared/install-scripts";

export interface CoordTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  plistPath: string;
}

interface InflightSnapshot {
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

interface RollbackPresent {
  db: boolean;
  key: boolean;
  authorizedKeys: boolean;
  handoff: boolean;
  plist: boolean;
}

interface CoordTargetRuntime {
  platform: string;
  gitSha: string;
  tailnetDnsName(): string;
  isLaunchAgentActive(label: string): Promise<boolean>;
  restoreLaunchAgent(label: string, plistPath: string): Promise<void>;
}

const defaultRuntime: CoordTargetRuntime = {
  platform: process.platform,
  gitSha: process.env.GIT_SHA ?? process.env.ROOST_GIT_SHA ?? "dev",
  tailnetDnsName: resolveTailnetDnsName,
  async isLaunchAgentActive(label) {
    const child = Bun.spawn(["launchctl", "print", `gui/${process.getuid?.() ?? 0}/${label}`], { stdout: "ignore", stderr: "ignore" });
    return await child.exited === 0;
  },
  async restoreLaunchAgent(label, plistPath) {
    const uid = process.getuid?.() ?? 0;
    const bootout = Bun.spawn(["launchctl", "bootout", `gui/${uid}/${label}`], { stdout: "ignore", stderr: "ignore" });
    await bootout.exited;
    const bootstrap = Bun.spawn(["launchctl", "bootstrap", `gui/${uid}`, plistPath], { stdout: "ignore", stderr: "ignore" });
    if (await bootstrap.exited !== 0) throw new Error("failed to restore prior coordinator LaunchAgent");
  },
};

function coordinatorKeyFingerprint(pem: Uint8Array): string {
  const encoded = Buffer.from(pem).toString("utf8").replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const raw = Buffer.from(encoded, "base64");
  const magic = Buffer.from("openssh-key-v1\0", "ascii");
  if (!raw.subarray(0, magic.length).equals(magic)) throw new Error("coordinator key is not an OpenSSH key");
  let position = magic.length;
  const readU32 = (): number => {
    const value = raw.readUInt32BE(position);
    position += 4;
    return value;
  };
  const readString = (): Buffer => {
    const length = readU32();
    const value = raw.subarray(position, position + length);
    position += length;
    return value;
  };
  readString(); readString(); readString(); readU32(); readString();
  const privateBlock = readString();
  let privatePosition = 8; // OpenSSH checkints.
  const readPrivateString = (): Buffer => {
    const length = privateBlock.readUInt32BE(privatePosition);
    privatePosition += 4;
    const value = privateBlock.subarray(privatePosition, privatePosition + length);
    privatePosition += length;
    return value;
  };
  if (readPrivateString().toString("utf8") !== "ssh-ed25519") throw new Error("coordinator key is not ed25519");
  const publicKey = readPrivateString();
  if (publicKey.length !== 32) throw new Error("coordinator key has an invalid public key");
  return createHash("sha256").update(publicKey).digest("hex");
}

function existingDirectory(path: string): string {
  let candidate = path;
  while (!fs.existsSync(candidate)) {
    const parent = dirname(candidate);
    if (parent === candidate) throw new Error(`no existing directory for ${path}`);
    candidate = parent;
  }
  return candidate;
}

function assertWritableDirectory(path: string, mustExist = false): void {
  if (mustExist && !fs.existsSync(path)) throw new Error(`${path} does not exist`);
  const directory = existingDirectory(path);
  if (!fs.statSync(directory).isDirectory()) throw new Error(`${directory} is not a directory`);
  fs.accessSync(directory, fs.constants.W_OK);
}

/** Target-side staging: no copied state becomes live before full validation. */
export class CoordTarget {
  #inflight: InflightSnapshot | null = null;
  #prepared: PreparedTarget | null = null;

  constructor(private readonly paths: CoordTargetPaths, private readonly runtime: CoordTargetRuntime = defaultRuntime) {}

  async prepare(request: {
    handoff_id: string;
    source_url: string;
    target_url: string;
    expected_coord_kid: string;
    action: "CHECK" | "PREPARE";
    estimated_db_size: bigint;
    expected_git_sha: string;
  }): Promise<void> {
    if (this.runtime.platform !== "darwin") throw new Error("Automatic coordinator moves currently require macOS.");
    if (this.runtime.gitSha !== request.expected_git_sha) {
      throw new Error(`worker version ${this.runtime.gitSha} does not match coordinator`);
    }
    const target = new URL(request.target_url);
    const localDnsName = this.runtime.tailnetDnsName().toLowerCase().replace(/\.$/, "");
    if (
      target.protocol !== "https:" || target.port !== "4102" || !target.hostname.endsWith(".ts.net") ||
      !localDnsName || target.hostname.toLowerCase() !== localDnsName
    ) {
      throw new Error("target URL does not match this worker's Tailscale address");
    }
    assertWritableDirectory(this.paths.dataDir);
    assertWritableDirectory(dirname(this.paths.plistPath), true);
    const stat = fs.statfsSync(existingDirectory(this.paths.dataDir));
    const available = Number(stat.bavail) * Number(stat.bsize);
    const required = Number(request.estimated_db_size) * 2 + 256 * 1024 * 1024;
    if (available < required) throw new Error(`insufficient disk: required ${required}, available ${available}`);
    await this.assertNoActiveCoordinator();
    if (request.action === "CHECK") return;

    fs.mkdirSync(this.paths.dataDir, { recursive: true, mode: 0o700 });

    const handoffDir = join(this.paths.dataDir, "handoffs", request.handoff_id);
    fs.mkdirSync(handoffDir, { recursive: true, mode: 0o700 });
    this.#prepared = {
      handoffId: request.handoff_id,
      sourceUrl: request.source_url,
      targetUrl: request.target_url,
      expectedCoordKid: request.expected_coord_kid,
      expectedGitSha: request.expected_git_sha,
    };
    // Preserve the pre-move landing before write-plist can replace it.
    this.captureRollback(join(handoffDir, "rollback"));
    await this.captureServeConfig(join(handoffDir, "rollback"));
    await this.runInstaller("write-plist", request.handoff_id);
    if (!process.env.ROOST_EXEC_BIN) await this.buildSourceSpa();
  }

  startSnapshot(request: {
    request_id: string;
    handoff_id: string;
    total_size: bigint;
    sha256: string;
    coord_key_pem: Uint8Array;
    authorized_keys: Uint8Array;
    secret_sha256: string;
    expected_worker_fps: string[];
  }): void {
    if (this.#inflight || !this.#prepared || this.#prepared.handoffId !== request.handoff_id) {
      throw new Error("coordinator target was not prepared");
    }
    const dir = join(this.paths.dataDir, "handoffs", request.handoff_id);
    const file = join(dir, "coordinator_v2.snapshot");
    fs.rmSync(file, { force: true });
    fs.writeFileSync(join(dir, "ssh_ed25519.key"), request.coord_key_pem, { mode: 0o600 });
    fs.writeFileSync(join(dir, "authorized_keys.roost"), request.authorized_keys, { mode: 0o600 });
    fs.writeFileSync(join(dir, "target-handoff.json"), JSON.stringify({
      version: 1, handoff_id: request.handoff_id, role: "TARGET", phase: "WAITING_FOR_WORKERS",
      source_url: this.#prepared.sourceUrl, target_url: this.#prepared.targetUrl, target_worker_fp: "target-pending",
      expected_worker_fps: request.expected_worker_fps, commit_acked_worker_fps: [],
      expected_coord_kid: this.#prepared.expectedCoordKid, expected_git_sha: this.#prepared.expectedGitSha,
      secret_sha256: request.secret_sha256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
    }), { mode: 0o600 });
    this.fsyncDirectory(dir);
    this.#inflight = {
      handoffId: request.handoff_id, file, fd: fs.openSync(file, "wx", 0o600),
      expectedSize: Number(request.total_size), expectedSha256: request.sha256,
      nextSeq: 0, received: 0, hasher: new Bun.CryptoHasher("sha256"),
    };
  }

  async appendSnapshot(chunk: { handoff_id: string; seq: number; data: Uint8Array; last: boolean }): Promise<void> {
    const inflight = this.#inflight;
    if (!inflight || inflight.handoffId !== chunk.handoff_id) throw new Error("unknown coordinator snapshot");
    const dir = dirname(inflight.file);
    try {
      if (chunk.seq !== inflight.nextSeq) {
        throw new Error(`snapshot chunk out of order: expected ${inflight.nextSeq}, got ${chunk.seq}`);
      }
      let offset = 0;
      while (offset < chunk.data.length) {
        const written = fs.writeSync(inflight.fd, chunk.data, offset, chunk.data.length - offset);
        if (written === 0) throw new Error("coordinator snapshot write made no progress");
        offset += written;
      }
      inflight.hasher.update(chunk.data);
      inflight.received += chunk.data.length;
      inflight.nextSeq++;
      if (!chunk.last) return;

      fs.fsyncSync(inflight.fd);
      fs.closeSync(inflight.fd);
      this.#inflight = null;
      const actualSha256 = inflight.hasher.digest("hex");
      if (inflight.received !== inflight.expectedSize || actualSha256 !== inflight.expectedSha256) {
        throw new Error("coordinator snapshot checksum mismatch");
      }
      const check = new Database(inflight.file, { readonly: true });
      try {
        const integrity = check.query("PRAGMA integrity_check").get() as { integrity_check?: string } | null;
        if (integrity?.integrity_check !== "ok") throw new Error("coordinator snapshot integrity check failed");
      } finally {
        check.close();
      }
      if (coordinatorKeyFingerprint(fs.readFileSync(join(dir, "ssh_ed25519.key"))) !== this.#prepared?.expectedCoordKid) {
        throw new Error("coordinator key fingerprint does not match source");
      }

      fs.renameSync(inflight.file, this.paths.dbPath);
      fs.renameSync(join(dir, "ssh_ed25519.key"), this.paths.keyPath);
      fs.renameSync(join(dir, "authorized_keys.roost"), this.paths.authorizedKeysPath);
      fs.renameSync(join(dir, "target-handoff.json"), this.paths.handoffPath);
      this.fsyncDirectory(dirname(this.paths.dbPath));
      await this.runInstaller("install", inflight.handoffId);
    } catch (error) {
      if (this.#inflight === inflight) {
        try { fs.closeSync(inflight.fd); } catch { /* already closed */ }
        this.#inflight = null;
      }
      try {
        await this.abort(inflight.handoffId);
        for (const file of ["coordinator_v2.snapshot", "ssh_ed25519.key", "authorized_keys.roost", "target-handoff.json"]) {
          fs.rmSync(join(dir, file), { force: true });
        }
        this.fsyncDirectory(dir);
      } catch (rollbackError) {
        throw new Error(`${(error as Error).message}; target rollback failed: ${(rollbackError as Error).message}`);
      }
      throw error;
    }
  }

  async abort(handoffId: string): Promise<void> {
    const rollback = join(this.paths.dataDir, "handoffs", handoffId, "rollback");
    let present: RollbackPresent;
    try { present = JSON.parse(fs.readFileSync(join(rollback, "present.json"), "utf8")) as RollbackPresent; } catch { return; }
    await this.runInstaller("uninstall", handoffId);
    for (const [canonical, name, existed] of [
      [this.paths.dbPath, "coordinator_v2.db", present.db],
      [this.paths.keyPath, "ssh_ed25519.key", present.key],
      [this.paths.authorizedKeysPath, "authorized_keys.roost", present.authorizedKeys],
      [this.paths.handoffPath, "coord-handoff.json", present.handoff],
      [this.paths.plistPath, "coordinator.plist", present.plist],
    ] as const) {
      const backup = join(rollback, name);
      if (existed) fs.copyFileSync(backup, canonical);
      else fs.rmSync(canonical, { force: true });
    }
    this.fsyncDirectory(dirname(this.paths.dbPath));
    await this.restoreServeConfig(rollback);
    if (present.plist) await this.runtime.restoreLaunchAgent(process.env.ROOST_COORD_LABEL ?? "com.roost.coordinator-v2", this.paths.plistPath);
  }

  private captureRollback(rollback: string): void {
    if (fs.existsSync(join(rollback, "present.json"))) return;
    fs.mkdirSync(rollback, { recursive: true, mode: 0o700 });
    const present: RollbackPresent = {
      db: fs.existsSync(this.paths.dbPath), key: fs.existsSync(this.paths.keyPath),
      authorizedKeys: fs.existsSync(this.paths.authorizedKeysPath), handoff: fs.existsSync(this.paths.handoffPath),
      plist: fs.existsSync(this.paths.plistPath),
    };
    for (const [canonical, name, exists] of [
      [this.paths.dbPath, "coordinator_v2.db", present.db],
      [this.paths.keyPath, "ssh_ed25519.key", present.key],
      [this.paths.authorizedKeysPath, "authorized_keys.roost", present.authorizedKeys],
      [this.paths.handoffPath, "coord-handoff.json", present.handoff],
      [this.paths.plistPath, "coordinator.plist", present.plist],
    ] as const) if (exists) fs.copyFileSync(canonical, join(rollback, name));
    fs.writeFileSync(join(rollback, "present.json"), JSON.stringify(present), { mode: 0o600 });
    this.fsyncDirectory(rollback);
  }

  private async captureServeConfig(rollback: string): Promise<void> {
    const config = join(rollback, "tailscale-serve.json");
    const child = Bun.spawn([process.env.ROOST_TAILSCALE_BIN ?? "tailscale", "serve", "get-config", config, "--all"], { stdout: "ignore", stderr: "ignore" });
    if (await child.exited !== 0) throw new Error("failed to snapshot Tailscale Serve configuration");
    fs.chmodSync(config, 0o600);
    this.fsyncDirectory(rollback);
  }

  private async restoreServeConfig(rollback: string): Promise<void> {
    const config = join(rollback, "tailscale-serve.json");
    const child = Bun.spawn([process.env.ROOST_TAILSCALE_BIN ?? "tailscale", "serve", "set-config", config, "--all"], { stdout: "ignore", stderr: "ignore" });
    if (await child.exited !== 0) throw new Error("failed to restore Tailscale Serve configuration");
  }

  private async assertNoActiveCoordinator(): Promise<void> {
    const label = process.env.ROOST_COORD_LABEL ?? "com.roost.coordinator-v2";
    if (await this.runtime.isLaunchAgentActive(label)) throw new Error("target already has an active coordinator");
  }

  private async buildSourceSpa(): Promise<void> {
    const child = Bun.spawn(["bun", "run", "build"], { cwd: join(process.cwd(), "apps", "web"), stdout: "ignore", stderr: "ignore" });
    if (await child.exited !== 0) throw new Error("target web build failed");
  }

  private async runInstaller(command: "write-plist" | "install" | "uninstall", handoffId: string): Promise<void> {
    const dir = join(this.paths.dataDir, "handoffs", handoffId);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const script = process.env.ROOST_COORDINATOR_INSTALL_SCRIPT ?? (COORD_INSTALL_SH ? join(dir, "install-coordinator.sh") : join(process.cwd(), "apps", "coord", "scripts", "install.sh"));
    if (COORD_INSTALL_SH && !process.env.ROOST_COORDINATOR_INSTALL_SCRIPT) fs.writeFileSync(script, COORD_INSTALL_SH, { mode: 0o700 });
    const child = Bun.spawn(["bash", script, command], {
      env: {
        ...process.env, ROOST_REPO_ROOT: process.cwd(), ROOST_COORD_DATA_DIR: this.paths.dataDir,
        ROOST_COORDINATOR_DB: this.paths.dbPath, ROOST_COORDINATOR_KEY_PATH: this.paths.keyPath,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: this.paths.authorizedKeysPath, ROOST_COORDINATOR_HANDOFF_PATH: this.paths.handoffPath,
        ROOST_COORDINATOR_PUBLIC_URL: this.#prepared?.targetUrl ?? process.env.ROOST_COORDINATOR_PUBLIC_URL,
        ROOST_COORD_PLIST: this.paths.plistPath,
      },
      stdout: "ignore", stderr: "ignore",
    });
    if (await child.exited !== 0) throw new Error(`coordinator ${command} failed`);
  }

  private fsyncDirectory(path: string): void {
    const fd = fs.openSync(path, "r");
    try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  }
}
