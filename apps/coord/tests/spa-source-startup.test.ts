// Pins the coordinator's startup report about its SPA source. A stamped
// ROOST_WEB_DIST_PATH that a later release settlement deleted otherwise shows
// up only as a 404 on every page while RPCs keep answering, so this boots the
// real entrypoint and requires the missing source to be stated once.
import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { WEB_ASSETS } from "@roost/host/web-embed";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
// One coordinator boot (migrations + listeners) plus two requests, well inside
// the unit tier's 30 s budget while leaving room for a loaded CI box.
const BOOT_BUDGET_MS = 30_000;

/** Read the coordinator's own `listening` line, which carries the port an
 *  ephemeral bind resolved to. */
async function waitForListeningOrigin(stdout: ReadableStream<Uint8Array>): Promise<string> {
  const decoder = new TextDecoder();
  let seen = "";
  for await (const chunk of stdout) {
    seen += decoder.decode(chunk, { stream: true });
    const listening = /"msg":"listening","bind":"([^"]+)"/.exec(seen);
    if (listening) return `http://${listening[1]}`;
  }
  throw new Error("coordinator exited before it reported a listener");
}

test("a retired web dist is reported once at startup, not only as a page 404", async () => {
  // Source checkouts carry the embedded-manifest stub (`gen-embed --stub`), so
  // a missing dist leaves the coordinator with no SPA at all — the state this
  // report exists for.
  expect(WEB_ASSETS.size).toBe(0);
  const workdir = mkdtempSync(join(tmpdir(), "roost-coord-spa-"));
  const retiredDist = join(workdir, "releases", "deleted-release", "apps", "web", "dist");
  const coordinator = Bun.spawn({
    cmd: [process.execPath, "--env-file=/dev/null", "apps/coord/src/main.ts"],
    cwd: REPO_ROOT,
    env: {
      PATH: process.env.PATH ?? "",
      HOME: workdir,
      ROOST_COORDINATOR_BIND: "127.0.0.1:0",
      ROOST_COORDINATOR_DB: join(workdir, "coordinator_v2.db"),
      ROOST_COORD_DATA_DIR: workdir,
      ROOST_COORD_LOG_DIR: workdir,
      ROOST_WEB_DIST_PATH: retiredDist,
      ROOST_DIAG: "0",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const stderrText = new Response(coordinator.stderr).text();
  try {
    const origin = await waitForListeningOrigin(coordinator.stdout);

    // The defect's signature: RPCs stay healthy while every page misses.
    const identity = await fetch(`${origin}/roost.v1.CoordinatorService/AuthCoordIdentity`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    expect(identity.status).toBe(200);
    expect((await fetch(`${origin}/`)).status).toBe(404);
  } finally {
    coordinator.kill();
    await coordinator.exited;
  }

  const reported = (await stderrText)
    .split("\n")
    .filter((line) => line.includes('"spa_source_missing"'))
    .map((line) => JSON.parse(line) as { level: string; web_dist_path: string });
  expect(reported).toHaveLength(1);
  expect(reported[0]?.level).toBe("error");
  expect(reported[0]?.web_dist_path).toBe(retiredDist);

  rmSync(workdir, { recursive: true, force: true });
}, BOOT_BUDGET_MS);
