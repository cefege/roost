// The supervised Mecatl daemon must prove readiness rather than assume it, and
// must never hand a caller an endpoint it cannot serve. A stale readiness file
// left by a previous run, an API major this worker cannot relay, and a daemon
// that died all have to read as unavailable with the reason an operator needs.

import { afterEach, expect, test } from "bun:test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createMecatlDaemon,
  type MecatlDaemon,
  type MecatlDaemonState,
} from "../src/mecatl/daemon.ts";
import { loadWorkerConfig } from "../src/config.ts";

const TEST_ROOT = join(tmpdir(), `roost-test-mecatl-daemon-${process.pid}`);
const BIN_DIR = join(TEST_ROOT, "bin");
const started: MecatlDaemon[] = [];

function fakeMecated(name: string, body: string): string {
  mkdirSync(BIN_DIR, { recursive: true });
  const path = join(BIN_DIR, name);
  writeFileSync(path, `#!/usr/bin/env bun\n${body}\n`, { mode: 0o755 });
  return path;
}

/** A daemon that publishes its readiness document and answers the
 *  compatibility probe with the supplied API major. */
const SERVING = `
const argv = Bun.argv.slice(2);
const opt = (name) => { const i = argv.indexOf(name); return i === -1 ? undefined : argv[i + 1]; };
const port = Number(opt("--http-addr").split(":")[1]);
const apiMajor = Number(Bun.env.FAKE_API_MAJOR ?? "1");
const server = Bun.serve({ hostname: "127.0.0.1", port, fetch(req) {
  if (req.headers.get("authorization") !== "Bearer " + Bun.env.MECATL_AUTH_TOKEN) {
    return new Response("unauthorized", { status: 401 });
  }
  const url = new URL(req.url);
  if (url.pathname === "/v1/compatibility") return Response.json({ api_major: apiMajor });
  return Response.json({ path: url.pathname });
}});
if (Bun.env.FAKE_STALE_READY === "1") {
  Bun.write(opt("--ready-file"), JSON.stringify({ pid: process.pid + 99_000 }));
} else {
  Bun.write(opt("--ready-file"), JSON.stringify({ pid: process.pid, api_major: apiMajor }));
}
const reader = Bun.stdin.stream().getReader();
(async () => { while (true) { const { done } = await reader.read(); if (done) break; } server.stop(true); process.exit(0); })();
`;

function daemonFor(env: Record<string, string | undefined>): MecatlDaemon {
  const configEnv = {
    ROOST_COORDINATOR_URL: "http://127.0.0.1:4103",
    ROOST_WORKER_KEY_PATH: join(TEST_ROOT, "key"),
    ROOST_WORKER_DATA_DIR: join(TEST_ROOT, "data"),
    ROOST_WORKER_LOG_DIR: join(TEST_ROOT, "logs"),
    ...env,
  };
  const daemon = createMecatlDaemon(loadWorkerConfig(configEnv, "linux"), configEnv);
  started.push(daemon);
  return daemon;
}
function readyEndpoint(daemon: MecatlDaemon): { baseUrl: string; pid: number } {
  const state = daemon.state();
  if (state.kind !== "ready") throw new Error(`daemon not ready: ${state.kind}`);
  return { baseUrl: state.baseUrl, pid: state.pid };
}

/** Supervision is a real subprocess reaching real readiness, so this polls the
 *  platform clock rather than a fake one: the awaited condition is the
 *  daemon's own published state, and every assertion names it. */
async function settle(
  daemon: MecatlDaemon,
  predicate: (state: MecatlDaemonState) => boolean,
  timeoutMs = 20_000,
): Promise<MecatlDaemonState> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const state = daemon.state();
    if (predicate(state)) return state;
    await Bun.sleep(25);
  }
  return daemon.state();
}

afterEach(async () => {
  while (started.length > 0) await started.pop()!.stop();
  rmSync(TEST_ROOT, { recursive: true, force: true });
});

test("an opted-out machine starts no daemon and refuses relayed requests", async () => {
  const daemon = daemonFor({});
  expect(daemon.state()).toEqual({ kind: "disabled" });
  await expect(
    daemon.request({ method: "GET", path: "/v1/sessions", headers: {} }),
  ).rejects.toThrow("disabled");
});

test("a serving daemon becomes ready and only then serves authenticated requests", async () => {
  const daemon = daemonFor({
    ROOST_MECATL: "1",
    ROOST_MECATL_BIN: fakeMecated("mecated-serving", SERVING),
    ROOST_MECATL_ROOT: TEST_ROOT,
  });
  const state = await settle(daemon, (s) => s.kind === "ready");
  expect(state.kind).toBe("ready");

  const response = await daemon.request({
    method: "GET",
    path: "/v1/sessions",
    headers: { accept: "application/json" },
  });
  expect(response.status).toBe(200);
  // The bearer is added by the daemon module; a caller supplies none and the
  // fake rejects anything else with 401.
  expect(await response.json()).toEqual({ path: "/v1/sessions" });
});

test("a readiness document from another process never counts as ready", async () => {
  const daemon = daemonFor({
    ROOST_MECATL: "1",
    ROOST_MECATL_BIN: fakeMecated("mecated-stale", `Bun.env.FAKE_STALE_READY = "1";\n${SERVING}`),
    ROOST_MECATL_ROOT: TEST_ROOT,
  });
  const state = await settle(daemon, (s) => s.kind === "unavailable", 15_000);
  expect(state).toEqual({ kind: "unavailable", reason: "readiness_timeout" });
}, 30_000);

test("an unrelayable API major is refused instead of half-supported", async () => {
  const daemon = daemonFor({
    ROOST_MECATL: "1",
    ROOST_MECATL_BIN: fakeMecated("mecated-v2", `Bun.env.FAKE_API_MAJOR = "2";\n${SERVING}`),
    ROOST_MECATL_ROOT: TEST_ROOT,
  });
  const state = await settle(daemon, (s) => s.kind === "unavailable");
  expect(state).toEqual({ kind: "unavailable", reason: "incompatible_api" });
});

test("a killed daemon leaves ready, restarts, and serves again", async () => {
  const daemon = daemonFor({
    ROOST_MECATL: "1",
    ROOST_MECATL_BIN: fakeMecated("mecated-restart", SERVING),
    ROOST_MECATL_ROOT: TEST_ROOT,
  });
  await settle(daemon, (s) => s.kind === "ready");
  const firstPid = readyEndpoint(daemon).pid;

  process.kill(firstPid, "SIGKILL");
  expect((await settle(daemon, (s) => s.kind !== "ready")).kind).not.toBe("ready");

  await settle(daemon, (s) => s.kind === "ready");
  expect(readyEndpoint(daemon).pid).not.toBe(firstPid);
  const response = await daemon.request({ method: "GET", path: "/v1/sessions", headers: {} });
  expect(response.status).toBe(200);
}, 30_000);

test("stopping the daemon ends the process and stops reporting ready", async () => {
  const daemon = daemonFor({
    ROOST_MECATL: "1",
    ROOST_MECATL_BIN: fakeMecated("mecated-stop", SERVING),
    ROOST_MECATL_ROOT: TEST_ROOT,
  });
  await settle(daemon, (s) => s.kind === "ready");
  const pid = readyEndpoint(daemon).pid;

  await daemon.stop();
  expect(daemon.state().kind).not.toBe("ready");
  expect(() => process.kill(pid, 0)).toThrow();
});
