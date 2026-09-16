// Supervises one loopback-bound Mecatl daemon (`mecated serve`) for this
// machine and owns the only copy of its bearer token. Started from main.ts
// boot when the operator opted this worker in; read by the relay executor,
// which forwards coordinator-relayed HTTP to it. Depends on worker config,
// shared paths, and the daemon's own readiness file.

import { randomBytes } from "node:crypto";
import { mkdirSync, openSync, closeSync, rmSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { log } from "@roost/shared/log";
import { workerDataDir, workerLogDir } from "@roost/shared/paths";
import type { MecatlUnavailableReason } from "@roost/shared/mecatl-runtime";
import type { WorkerConfig } from "../config.ts";

export type { MecatlUnavailableReason } from "@roost/shared/mecatl-runtime";

export type MecatlDaemonState =
  | { kind: "disabled" }
  | { kind: "starting" }
  | { kind: "ready"; baseUrl: string; pid: number }
  | { kind: "unavailable"; reason: MecatlUnavailableReason };

export interface MecatlLocalRequest {
  method: string;
  path: string;
  headers: Readonly<Record<string, string>>;
  body?: Uint8Array;
  signal?: AbortSignal;
}

export interface MecatlDaemon {
  state(): MecatlDaemonState;
  /** Perform one request against the local daemon. The bearer is added here
   *  and nowhere else, so no caller can leak or forward it. */
  request(init: MecatlLocalRequest): Promise<Response>;
  stop(): Promise<void>;
}

/** The API major this worker knows how to relay. A daemon reporting anything
 *  else is refused rather than half-supported: the relay is transparent, so a
 *  breaking API change would surface as unexplained browser errors. */
const SUPPORTED_API_MAJOR = 1;
const READY_POLL_MS = 100;
const READY_TIMEOUT_MS = 10_000;
const STOP_TIMEOUT_MS = 10_000;
const RESTART_BACKOFF_MS = [1_000, 2_000, 5_000, 5_000, 5_000] as const;
const RESTART_WINDOW_MS = 5 * 60_000;

/** `env` resolves the worker's data directory and is the same map the config
 *  was loaded from, so a caller that supplied one does not get a daemon whose
 *  session store lands in the ambient user's directory. */
export function createMecatlDaemon(
  cfg: WorkerConfig,
  env: Record<string, string | undefined> = process.env as Record<string, string | undefined>,
): MecatlDaemon {
  const supervisor = new MecatlSupervisor(cfg, env);
  supervisor.start();
  return supervisor;
}

class MecatlSupervisor implements MecatlDaemon {
  #cfg: WorkerConfig;
  #env: Record<string, string | undefined>;
  #state: MecatlDaemonState;
  #token = "";
  #child: Bun.Subprocess<"pipe", "ignore", number> | null = null;
  #recentStarts: number[] = [];
  #stopping = false;
  #restartTimer: Timer | undefined;
  /** Path of the current attempt's gRPC socket, unlinked when it dies: a
   *  per-attempt name would otherwise litter the temp dir on every restart. */
  #socketPath: string | null = null;

  constructor(cfg: WorkerConfig, env: Record<string, string | undefined>) {
    this.#cfg = cfg;
    this.#env = env;
    this.#state = cfg.mecatlEnabled ? { kind: "starting" } : { kind: "disabled" };
  }

  state(): MecatlDaemonState {
    return this.#state;
  }

  start(): void {
    if (!this.#cfg.mecatlEnabled) {
      log.info("worker", "mecatl_disabled", {});
      return;
    }
    this.#launchGuarded();
  }

  /** The agent runtime is optional, so nothing it does may reach the worker's
   *  unhandledRejection handler: that exits the process and takes every live
   *  PTY with it. Directory creation, port allocation and log-file opening all
   *  throw outside the spawn try block, which is exactly that path. */
  #launchGuarded(): void {
    void this.#launch().catch((error: unknown) => {
      log.error("worker", "mecatl_launch_failed", { error: String(error) });
      this.#fail("spawn_failed");
    });
  }

  async request(init: MecatlLocalRequest): Promise<Response> {
    const current = this.#state;
    if (current.kind !== "ready") {
      throw new MecatlUnavailableError(unavailableReasonFor(current));
    }
    const headers = new Headers(init.headers);
    headers.set("authorization", `Bearer ${this.#token}`);
    return await fetch(`${current.baseUrl}${init.path}`, {
      method: init.method,
      headers,
      body: init.body ? (init.body.slice().buffer as ArrayBuffer) : undefined,
      signal: init.signal,
    });
  }

  async stop(): Promise<void> {
    this.#stopping = true;
    clearTimeout(this.#restartTimer);
    const child = this.#child;
    this.#child = null;
    if (!child) return;
    // Closing the inherited stdin is Mecatl's documented parent-liveness EOF:
    // it drains listeners and persists session state, which SIGKILL would not.
    try { child.stdin.end(); } catch { /* already closed */ }
    const exited = await Promise.race([
      child.exited,
      Bun.sleep(STOP_TIMEOUT_MS).then(() => null),
    ]);
    if (exited === null) {
      log.warn("worker", "mecatl_stop_timeout", { pid: child.pid });
      child.kill("SIGKILL");
      await child.exited;
    }
    this.#reapSocket();
    this.#state = { kind: "unavailable", reason: "stopped" };
    log.info("worker", "mecatl_stopped", { pid: child.pid });
  }

  async #launch(): Promise<void> {
    if (this.#stopping) return;
    const binary = this.#cfg.mecatlBin ?? Bun.which("mecated");
    if (!binary) {
      this.#fail("binary_missing");
      return;
    }
    this.#recentStarts = [
      ...this.#recentStarts.filter((at) => Date.now() - at < RESTART_WINDOW_MS),
      Date.now(),
    ];

    const stateDir = join(workerDataDir(this.#env), "mecatl");
    const readyFile = join(stateDir, "ready.json");
    const socketDir = join(tmpdir(), "roost-mecatl");
    // Darwin caps a UNIX socket path at 103 bytes and the worker data dir is
    // long on macOS, so the gRPC socket lives under the temp dir instead. The
    // relay only ever speaks HTTP; this listener exists so the daemon does not
    // claim the default TCP 8080 an operator's own mecated may hold.
    //
    // The name is per attempt, not per worker: Mecatl refuses to replace a
    // LIVE listener, and a restart fires on backoff without waiting for the
    // previous daemon to finish draining, so a fixed name would make every
    // retry after a readiness failure refuse until the budget ran out.
    const grpcSocket = join(socketDir, `${process.pid}-${randomBytes(6).toString("hex")}.sock`);
    mkdirSync(stateDir, { recursive: true });
    mkdirSync(socketDir, { recursive: true, mode: 0o700 });
    mkdirSync(this.#cfg.logDir, { recursive: true });

    const port = allocateLoopbackPort();
    this.#token = randomBytes(32).toString("hex");
    const stderr = openSync(join(this.#cfg.logDir, "mecatl.err.log"), "a", 0o644);
    let child: Bun.Subprocess<"pipe", "ignore", number>;
    try {
      child = Bun.spawn({
        cmd: [
          binary,
          "serve",
          "--http-addr", `127.0.0.1:${port}`,
          "--grpc-unix-socket", grpcSocket,
          "--metrics-addr", "",
          "--workspace", this.#cfg.mecatlRoot,
          "--store-dir", join(stateDir, "sessions"),
          "--ready-file", readyFile,
          "--lifetime-stdin",
        ],
        stdio: ["pipe", "ignore", stderr],
        // The token travels in the child environment, never in argv: argv is
        // world-readable in `ps`, which this worker itself scans every 250 ms.
        env: { ...(process.env as Record<string, string>), MECATL_AUTH_TOKEN: this.#token },
      });
    } catch (error) {
      closeSync(stderr);
      log.error("worker", "mecatl_spawn_failed", { error: String(error) });
      this.#fail("spawn_failed");
      return;
    }
    closeSync(stderr);
    this.#child = child;
    this.#socketPath = grpcSocket;
    this.#state = { kind: "starting" };
    log.info("worker", "mecatl_started", { pid: child.pid, port });

    void child.exited.then((code) => this.#onExit(child, code));

    const baseUrl = `http://127.0.0.1:${port}`;
    const ready = await this.#awaitReadiness(child, readyFile, baseUrl);
    if (ready !== null) {
      // This teardown is ours, so drop the identity before ending stdin: the
      // exit handler must not relabel a proven readiness failure as a crash
      // and must not race a second restart against the one scheduled here.
      this.#child = null;
      try { child.stdin.end(); } catch { /* already gone */ }
      this.#fail(ready);
      this.#scheduleRestart();
      return;
    }
    if (this.#child !== child) return;
    this.#state = { kind: "ready", baseUrl, pid: child.pid };
    log.info("worker", "mecatl_ready", { pid: child.pid, port });
  }

  /** Returns null when the daemon proved itself, or the reason it did not.
   *  Existence of the readiness file is not readiness: the file survives a
   *  previous run, so its pid must match and the API must answer. */
  async #awaitReadiness(
    child: Bun.Subprocess,
    readyFile: string,
    baseUrl: string,
  ): Promise<MecatlUnavailableReason | null> {
    const deadline = Date.now() + READY_TIMEOUT_MS;
    let published = false;
    while (Date.now() < deadline) {
      if (this.#child !== child || child.killed) return "daemon_exit";
      if (!published) {
        try {
          const document = JSON.parse(await readFile(readyFile, "utf8")) as { pid?: number };
          published = document.pid === child.pid;
        } catch {
          published = false;
        }
      }
      if (published) {
        const compatible = await this.#probeCompatibility(baseUrl);
        if (compatible === true) return null;
        if (compatible === "incompatible") return "incompatible_api";
      }
      await Bun.sleep(READY_POLL_MS);
    }
    return "readiness_timeout";
  }

  async #probeCompatibility(baseUrl: string): Promise<true | "incompatible" | false> {
    try {
      const response = await fetch(`${baseUrl}/v1/compatibility`, {
        headers: { authorization: `Bearer ${this.#token}` },
        signal: AbortSignal.timeout(2_000),
      });
      if (!response.ok) return false;
      const info = await response.json() as { api_major?: number };
      return info.api_major === SUPPORTED_API_MAJOR ? true : "incompatible";
    } catch {
      return false;
    }
  }

  #onExit(child: Bun.Subprocess, code: number | null): void {
    if (this.#child !== child) return;
    this.#child = null;
    log.warn("worker", "mecatl_exit", { pid: child.pid, exit_code: code });
    this.#reapSocket();
    this.#fail("daemon_exit");
    this.#scheduleRestart();
  }

  /** Retry the launch unless the operator turned the runtime off, the worker
   *  is shutting down, or this machine has already spent its restart budget.
   *  The current failure reason is left in place so an operator sees WHY the
   *  daemon is down while the backoff runs. */
  #scheduleRestart(): void {
    if (this.#stopping || !this.#cfg.mecatlEnabled) return;
    const attempt = this.#recentStarts.filter(
      (at) => Date.now() - at < RESTART_WINDOW_MS,
    ).length;
    if (attempt >= RESTART_BACKOFF_MS.length) {
      this.#fail("restart_exhausted");
      return;
    }
    const delay = RESTART_BACKOFF_MS[attempt] ?? RESTART_BACKOFF_MS.at(-1)!;
    this.#restartTimer = setTimeout(() => { this.#launchGuarded(); }, delay);
    this.#restartTimer.unref?.();
  }

  #fail(reason: MecatlUnavailableReason): void {
    this.#state = { kind: "unavailable", reason };
    log.warn("worker", "mecatl_unavailable", { reason });
  }

  /** A dead daemon's socket is inert, and the next attempt mints its own. */
  #reapSocket(): void {
    const path = this.#socketPath;
    this.#socketPath = null;
    if (path) rmSync(path, { force: true });
  }
}

export class MecatlUnavailableError extends Error {
  readonly reason: MecatlUnavailableReason;
  constructor(reason: MecatlUnavailableReason) {
    super(`mecatl unavailable: ${reason}`);
    this.reason = reason;
  }
}

/** The reason a non-ready state gives a caller. Exported because the relay
 *  reports the same vocabulary to the coordinator, and a machine the operator
 *  never opted in must not read as a crashed one. */
export function unavailableReasonFor(
  state: MecatlDaemonState,
): MecatlUnavailableReason {
  switch (state.kind) {
    case "disabled": return "disabled";
    case "unavailable": return state.reason;
    // A daemon still proving itself has no endpoint to relay to yet, and that
    // is a wait, not a crash: the pane must not tell an operator to reinstall
    // during the ordinary cold-boot readiness window.
    case "starting": return "not_ready";
    case "ready": return "daemon_exit";
  }
}

/** Bind and immediately release a loopback port so the daemon can be told
 *  exactly where to listen. Mecatl's readiness document reports its gRPC
 *  address, not its HTTP one, so the worker owns the number instead of
 *  discovering it. */
function allocateLoopbackPort(): number {
  const listener = Bun.listen({
    hostname: "127.0.0.1",
    port: 0,
    socket: { data() { /* never accepts: closed below */ } },
  });
  const { port } = listener;
  listener.stop(true);
  return port;
}
