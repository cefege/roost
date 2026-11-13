import { constants } from "node:fs";
import { access, mkdir, mkdtemp, readFile, realpath, rm, stat, symlink, writeFile } from "node:fs/promises";
import { basename, delimiter, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import type { Page } from "playwright";
import {
  createHome, eventually, execute, freeLoopbackPort, isolatedEnvironment,
  startLogged, type RunningChild,
} from "./runtime";
import { captureKeeperRoot, captureStackRoot, stopStackProcesses, type StackProcessIdentity } from "./stack-processes";
import { assertOwned } from "./profile-manifest";
import { allowOwnedLoopbackOrigin } from "./network-guard";

export interface StackOptions {
  launcherPath: string;
  ompEnvironment: Record<string, string>;
  roostPath?: string;
  keeperPath?: string;
  webDistPath?: string;
  workerHome?: string;
  workerCwd?: string;
}

export interface RoostStack {
  origin: string;
  workerFp: string;
  home: string;
  env: Record<string, string>;
  pair(page: Page): Promise<void>;
  openConversation(page: Page, workspace: string): Promise<string>;
  stop(): Promise<void>;
}

const STARTUP_TIMEOUT = 60_000;
const WORKER_LABEL = "omp-parity";

async function waitForStack<T>(
  label: string,
  children: readonly RunningChild[],
  observe: () => Promise<T | undefined>,
): Promise<T> {
  const result = await eventually<{ value: T } | { exited: RunningChild }>(label, async () => {
    const exited = children.find(child => child.exitCode !== null);
    if (exited) return { exited };
    const value = await observe();
    return value === undefined ? undefined : { value };
  }, STARTUP_TIMEOUT);
  if ("exited" in result) throw new Error(`${label}: process ${result.exited.pid} exited ${result.exited.exitCode}`);
  return result.value;
}

function pairingUrl(stdout: string, origin: string): string {
  const value = stdout.trim();
  const url = new URL(value);
  if (url.origin !== origin || url.pathname !== "/" || url.search || !/^#pair=[^\s#]+$/.test(url.hash)) {
    throw new Error("add-browser did not return one pairing URL for the isolated coordinator");
  }
  return url.href;
}

function onlineWorker(stdout: string): string | undefined {
  const [header, ...lines] = stdout.trimEnd().split("\n");
  if (header !== "fp\tlabel\tstate\tos") throw new Error("workers returned an unexpected roster header");
  let found: string | undefined;
  for (const line of lines) {
    const columns = line.split("\t");
    const [fingerprint, label, state, os] = columns;
    if (columns.length !== 4 || !fingerprint || !os || !["online", "offline"].includes(state ?? "")) {
      throw new Error("workers returned a malformed roster row");
    }
    if (label !== WORKER_LABEL || state !== "online") continue;
    if (found !== undefined) throw new Error("more than one isolated parity worker is online");
    found = fingerprint;
  }
  return found;
}

async function startupDiagnostics(paths: string[]): Promise<string> {
  const logs = await Promise.all(paths.map(async path => {
    try { return `${path}:\n${(await readFile(path, "utf8")).slice(-12_000)}`; }
    catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") return `${path}: not created`;
      return `${path}: unreadable (${String(error)})`;
    }
  }));
  return logs.join("\n");
}

async function createOwnedDirectory(ownedRoot: string, requested: string): Promise<string> {
  const target = resolve(ownedRoot, requested);
  assertOwned(ownedRoot, target);
  if (target === ownedRoot) throw new Error("Stack worker directories must be beneath the owned run root");
  let directory = ownedRoot;
  for (const segment of relative(ownedRoot, target).split(sep)) {
    const candidate = join(directory, segment);
    try { await mkdir(candidate, { mode: 0o700 }); }
    catch (error) {
      if (!(error instanceof Error && "code" in error && error.code === "EEXIST")) throw error;
    }
    directory = await realpath(candidate);
    assertOwned(ownedRoot, directory);
    if (directory === ownedRoot || !(await stat(directory)).isDirectory()) throw new Error(`Invalid owned worker directory: ${candidate}`);
  }
  return directory;
}

export async function startStack(repoRoot: string, runRoot: string, options: StackOptions): Promise<RoostStack> {
  const root = resolve(repoRoot);
  const roost = resolve(root, options.roostPath ?? "target/debug/roost");
  const keeper = resolve(root, options.keeperPath ?? "target/debug/roost-keeper");
  const webDist = resolve(root, options.webDistPath ?? "target/dx/roost-web/release/web/public");
  const launcher = resolve(root, options.launcherPath);
  if (basename(launcher) !== "omp") throw new Error("launcherPath must name the owned omp executable placed first on PATH");
  await Promise.all([access(roost, constants.X_OK), access(keeper, constants.X_OK), access(launcher, constants.X_OK), access(join(webDist, "index.html"))]);
  await mkdir(resolve(runRoot), { recursive: true });
  const ownedRoot = await realpath(runRoot);
  const stackRoot = join(ownedRoot, "stack");
  // Refuse reuse: neither an old enrollment nor an old conversation belongs to this run.
  await mkdir(stackRoot, { mode: 0o700 });
  const cliHome = join(stackRoot, "cli-home");
  const home = await createOwnedDirectory(ownedRoot, options.workerHome ?? join(stackRoot, "worker-home"));
  const workerCwd = options.workerCwd ? await createOwnedDirectory(ownedRoot, options.workerCwd) : home;
  await Promise.all([createHome(cliHome), createHome(home)]);
  const coordData = join(stackRoot, "coordinator");
  const workerData = join(stackRoot, "worker");
  const logs = join(stackRoot, "logs");
  await Promise.all([mkdir(coordData), mkdir(workerData), mkdir(logs), createOwnedDirectory(ownedRoot, join(home, "fixtures"))]);
  const coordPort = freeLoopbackPort();
  let workerPort = freeLoopbackPort();
  while (workerPort === coordPort) workerPort = freeLoopbackPort();
  const origin = `http://127.0.0.1:${coordPort}`;
  allowOwnedLoopbackOrigin(origin);
  const authorizedKeys = join(coordData, "authorized_keys.roost");
  await writeFile(authorizedKeys, "", { mode: 0o600, flag: "wx" });
  // Keep Unix socket paths below macOS's limit even when the artifact root is long.
  const socketRoot = await mkdtemp("/tmp/roost-omp-parity-");
  // The worker binds its agent-report socket inside its data directory; a short
  // link keeps that path bindable while the data stays in the run's artifacts.
  const workerDataLink = join(socketRoot, "worker");
  await symlink(workerData, workerDataLink, "dir");
  const controlled = {
    ROOST_COORD_DATA_DIR: coordData,
    ROOST_COORD_LOG_DIR: join(logs, "coordinator"),
    ROOST_COORDINATOR_LOG_DIR: join(logs, "coordinator"),
    ROOST_COORDINATOR_DB: join(coordData, "coordinator.sqlite"),
    ROOST_COORDINATOR_AUTHORIZED_KEYS: authorizedKeys,
    ROOST_COORDINATOR_BIND: `127.0.0.1:${coordPort}`,
    ROOST_COORDINATOR_URL: origin,
    ROOST_COORD_URL: origin,
    ROOST_CORS_ALLOWED_ORIGINS: `${origin},http://127.0.0.1:${workerPort}`,
    ROOST_WEB_DIST_PATH: webDist,
    ROOST_WORKER_DATA_DIR: workerDataLink,
    ROOST_WORKER_LOG_DIR: join(logs, "worker"),
    ROOST_WORKER_KEY_PATH: join(workerData, "worker.key"),
    ROOST_WORKER_LOCAL_UI_BIND: `127.0.0.1:${workerPort}`,
    ROOST_WORKER_LABEL: WORKER_LABEL,
    ROOST_KEEPER_EXECUTABLE: keeper,
    ROOST_KEEPER_SOCKET: join(socketRoot, "keeper.sock"),
    ROOST_KEEPER_PID_FILE: join(socketRoot, "keeper.pid"),
    ROOST_KEEPER_CAPABILITY_FILE: join(socketRoot, "keeper.capability"),
    ROOST_SERVICE_DIR: join(stackRoot, "services"),
    ROOST_VERSIONS_DIR: join(stackRoot, "versions"),
    ROOST_SKIP_AGENT_INTEGRATIONS: "1",
    ROOST_TERMINAL_PEER_ENABLED: "0",
    ROOST_TRUST_PROXY: "0",
    ROOST_RELAXED_CSP: "0",
    ROOST_AUTO_UPDATE: "0",
  };
  const env = isolatedEnvironment(cliHome, controlled);
  const workerEnv = {
    ...options.ompEnvironment,
    ...isolatedEnvironment(home),
    ...controlled,
    PATH: [dirname(launcher), options.ompEnvironment.PATH ?? env.PATH ?? ""].filter(Boolean).join(delimiter),
    SHELL: "/bin/bash",
  };
  const children: RunningChild[] = [];
  const processRoots: StackProcessIdentity[] = [];
  const coordLog = join(logs, "coordinator.stdout.log");
  const workerLog = join(logs, "worker.stdout.log");
  let keeperCaptured = false;
  let stopPromise: Promise<void> | undefined;
  const stop = (): Promise<void> => stopPromise ??= (async () => {
    let captureError: unknown;
    if (!keeperCaptured && children.length > 1) {
      try {
        const keeperRoot = await captureKeeperRoot(controlled.ROOST_KEEPER_PID_FILE, root, env);
        if (keeperRoot) processRoots.push(keeperRoot);
      } catch (error) {
        if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) captureError = error;
      }
    }
    await stopStackProcesses(children, processRoots, root, env);
    await rm(socketRoot, { recursive: true, force: true });
    if (captureError) throw captureError;
  })();
  const cli = (args: string[]) => execute([roost, ...args], root, env);
  try {
    const coordinator = await startLogged([roost, "coord"], root, env, coordLog);
    children.push(coordinator);
    processRoots.push(await captureStackRoot(coordinator.pid, root, env));
    await waitForStack("isolated coordinator readiness", children, async () => {
      const response = await fetch(`${origin}/readyz`, { signal: AbortSignal.timeout(1_000) });
      return response.ok ? true : undefined;
    });
    const cliPairing = pairingUrl(await cli(["add-browser", "--no-qr", "--label", "omp-parity-cli"]), origin);
    await cli(["api", "login", cliPairing, "--label", "omp-parity-cli"]);
    const token = (await cli(["api", "bootstrap-mint", "worker", "--label", WORKER_LABEL])).trim();
    if (!token || /\s/.test(token)) throw new Error("bootstrap-mint did not return one worker grant");
    const worker = await startLogged([roost, "worker", "--coordinator-url", origin], workerCwd, { ...workerEnv, ROOST_BOOTSTRAP_TOKEN: token }, workerLog);
    children.push(worker);
    processRoots.push(await captureStackRoot(worker.pid, root, env));
    processRoots.push(await waitForStack("isolated keeper startup", children,
      () => captureKeeperRoot(controlled.ROOST_KEEPER_PID_FILE, root, env)));
    keeperCaptured = true;
    const workerFp = await waitForStack("isolated worker routability", children,
      async () => onlineWorker(await cli(["api", "workers"])));
    return {
      origin, workerFp, home, env, stop,
      async pair(page) {
        const link = pairingUrl(await cli(["add-browser", "--no-qr", "--label", "omp-parity-browser"]), origin);
        await page.goto(link, { waitUntil: "domcontentloaded", timeout: STARTUP_TIMEOUT });
        await page.waitForURL(url => url.origin === origin && !url.hash, { timeout: STARTUP_TIMEOUT });
        await page.getByTestId("home-landing").waitFor({ state: "visible", timeout: STARTUP_TIMEOUT });
      },
      async openConversation(page, workspace) {
        const target = await realpath(workspace);
        const trail = relative(home, target);
        if (!trail || isAbsolute(trail) || trail === ".." || trail.startsWith(`..${sep}`)) {
          throw new Error("parity workspaces must be directories beneath the isolated worker HOME");
        }
        await page.goto(`${origin}/browse/${encodeURIComponent(workerFp)}?intent=omp`, { waitUntil: "domcontentloaded", timeout: STARTUP_TIMEOUT });
        await page.getByTestId("browse-page").waitFor({ state: "visible", timeout: STARTUP_TIMEOUT });
        await page.getByTestId("browse-home").click();
        let current = home;
        await page.getByTestId("browse-crumbs").getByTitle(current, { exact: true }).waitFor({ state: "visible", timeout: STARTUP_TIMEOUT });
        for (const segment of trail.split(sep)) {
          const row = page.getByTestId("browse-row").filter({ has: page.getByTitle(segment, { exact: true }) });
          await row.click({ timeout: STARTUP_TIMEOUT });
          current = join(current, segment);
          await page.getByTestId("browse-crumbs").getByTitle(current, { exact: true }).waitFor({ state: "visible", timeout: STARTUP_TIMEOUT });
        }
        await page.getByRole("button", { name: "New agent here", exact: true }).click();
        await page.waitForURL(url => url.origin === origin && /^\/a\/[^/]+$/.test(url.pathname), { timeout: STARTUP_TIMEOUT });
        const conversationId = decodeURIComponent(new URL(page.url()).pathname.slice(3));
        const editor = page.getByRole("textbox", { name: "Message OMP", exact: true });
        await editor.waitFor({ state: "visible", timeout: STARTUP_TIMEOUT });
        await eventually("native OMP composer ready", async () => await editor.isEditable() ? true : undefined, STARTUP_TIMEOUT);
        return conversationId;
      },
    };
  } catch (error) {
    const diagnostics = await startupDiagnostics([coordLog, workerLog]);
    try { await stop(); }
    catch (cleanupError) { throw new AggregateError([error, cleanupError], `Isolated Roost stack failed\n${diagnostics}`); }
    throw new Error(`Isolated Roost stack failed\n${diagnostics}`, { cause: error });
  }
}
