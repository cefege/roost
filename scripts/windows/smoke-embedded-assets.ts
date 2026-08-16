#!/usr/bin/env bun
import { chromium } from "@playwright/test";
import { mkdtempSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createServer } from "node:net";

function argument(name: string, fallback?: string): string {
  const index = process.argv.indexOf(name);
  if (index < 0) {
    if (fallback !== undefined) return fallback;
    throw new Error(`missing ${name}`);
  }
  const value = process.argv[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`missing value for ${name}`);
  return value;
}

async function availablePort(): Promise<number> {
  const { promise, resolve: resolvePort, reject } = Promise.withResolvers<number>();
  const server = createServer();
  server.once("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    if (!address || typeof address === "string") {
      server.close();
      reject(new Error("could not reserve a loopback port"));
      return;
    }
    const port = address.port;
    server.close((error) => error ? reject(error) : resolvePort(port));
  });
  return await promise;
}

async function readStream(stream: ReadableStream<Uint8Array> | number | null | undefined): Promise<string> {
  if (!stream || typeof stream === "number") return "";
  return await new Response(stream).text();
}

const binary = resolve(argument("--binary", "dist/roost-windows-x64.exe"));
const channel = argument("--channel", "chromium");
if (channel !== "chromium" && channel !== "msedge") {
  throw new Error(`unsupported browser channel: ${channel}`);
}
const artifactDir = resolve(argument("--artifact-dir", `test-results/windows-${channel}`));
mkdirSync(artifactDir, { recursive: true });
const stateDir = mkdtempSync(join(tmpdir(), "roost-windows-embed-"));
const port = await availablePort();
const origin = `http://127.0.0.1:${port}`;
const child = Bun.spawn({
  cmd: [binary, "coord"],
  env: {
    ...process.env,
    APPDATA: stateDir,
    LOCALAPPDATA: stateDir,
    ROOST_COORDINATOR_BIND: `127.0.0.1:${port}`,
    ROOST_COORDINATOR_DB: join(stateDir, "coordinator.db"),
    ROOST_COORDINATOR_KEY_PATH: join(stateDir, "coordinator.key"),
    ROOST_COORDINATOR_AUTHORIZED_KEYS: join(stateDir, "authorized_keys"),
    ROOST_COORDINATOR_HANDOFF_PATH: join(stateDir, "handoff.json"),
    ROOST_COORDINATOR_LOG_DIR: join(stateDir, "logs"),
  },
  stdin: "ignore",
  stdout: "pipe",
  stderr: "pipe",
});
const stdout = readStream(child.stdout);
const stderr = readStream(child.stderr);

try {
  const deadline = Date.now() + 60_000;
  let response: Response | undefined;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`Windows coordinator exited early (${child.exitCode})`);
    try {
      response = await fetch(`${origin}/`, { signal: AbortSignal.timeout(2_000) });
      if (response.ok) break;
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await Bun.sleep(200);
  }
  if (!response?.ok) throw new Error(`embedded coordinator did not become ready: ${String(lastError)}`);
  const html = await response.text();
  if (!/<!doctype html/i.test(html)) throw new Error("Windows binary did not serve its embedded index.html");

  const assetPaths = [...html.matchAll(/(?:src|href)=["']([^"']+)["']/g)]
    .map((match) => match[1]!)
    .filter((path) => path.startsWith("/assets/"));
  const uniqueAssets = [...new Set(assetPaths)];
  if (uniqueAssets.length === 0) throw new Error("embedded index.html referenced no generated assets");
  for (const path of uniqueAssets) {
    const asset = await fetch(new URL(path, origin));
    if (!asset.ok || Number(asset.headers.get("content-length") ?? "1") === 0) {
      throw new Error(`embedded asset failed: ${path} (${asset.status})`);
    }
    await asset.arrayBuffer();
  }

  const browser = await chromium.launch(channel === "msedge" ? { channel: "msedge" } : {});
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await page.goto(origin, { waitUntil: "domcontentloaded", timeout: 60_000 });
    await page.locator("body").waitFor({ state: "visible" });
    await page.screenshot({ path: join(artifactDir, "embedded-ui.png"), fullPage: true });
    await Bun.write(join(artifactDir, "embedded-assets.json"), JSON.stringify({ channel, origin, assets: uniqueAssets }, null, 2) + "\n");
  } finally {
    await browser.close();
  }
} catch (error) {
  await Bun.write(join(artifactDir, "failure.txt"), `${String(error)}\n`);
  throw error;
} finally {
  if (child.exitCode === null) child.kill();
  await child.exited;
  await Bun.write(join(artifactDir, "coordinator.stdout.log"), await stdout);
  await Bun.write(join(artifactDir, "coordinator.stderr.log"), await stderr);
  rmSync(stateDir, { recursive: true, force: true });
}
