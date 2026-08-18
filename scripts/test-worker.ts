import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";

const REPOSITORY_ROOT = dirname(import.meta.dir);
const KEEPER_ENTRY = "multiplexed-main.ts";
const KEEPER_SOCKET = "mux-keeper.sock";
const CHILD_TIMEOUT_MS = 90_000;
const GRACEFUL_STOP_MS = 5_000;
const FORCED_STOP_MS = 2_000;
const KEEPER_START_SETTLE_MS = 1_000;


function readKeeperPid(pidPath: string): number | null {
  try {
    const pid = Number.parseInt(readFileSync(pidPath, "utf8").trim(), 10);
    return Number.isSafeInteger(pid) && pid > 0 ? pid : null;
  } catch {
    return null;
  }
}

function isExpectedKeeper(pid: number, socketPath: string): boolean {
  const result = Bun.spawnSync(["ps", "-ww", "-p", String(pid), "-o", "command="]);
  if (result.exitCode !== 0) return false;
  const command = result.stdout.toString();
  return command.includes(KEEPER_ENTRY) && command.includes(socketPath);
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function stopKeeper(tempRoot: string): Promise<void> {
  const socketPath = join(tempRoot, "worker-data", KEEPER_SOCKET);
  const pid = readKeeperPid(`${socketPath}.pid`);
  if (pid === null || !isExpectedKeeper(pid, socketPath)) return;
  try { process.kill(pid, "SIGTERM"); } catch { return; }
  const deadline = Date.now() + GRACEFUL_STOP_MS;
  while (isAlive(pid) && Date.now() < deadline) await Bun.sleep(50);

  if (isAlive(pid) && isExpectedKeeper(pid, socketPath)) {
    try { process.kill(pid, "SIGKILL"); } catch { return; }
    const forcedDeadline = Date.now() + FORCED_STOP_MS;
    while (isAlive(pid) && Date.now() < forcedDeadline) await Bun.sleep(50);
  }

  if (isAlive(pid) && isExpectedKeeper(pid, socketPath)) {
    throw new Error(`keeper did not stop: pid=${pid} socket=${socketPath}`);
  }
}

async function cleanupTestRoot(tempRoot: string): Promise<void> {
  const pidPath = join(tempRoot, "worker-data", `${KEEPER_SOCKET}.pid`);
  const deadline = Date.now() + KEEPER_START_SETTLE_MS;
  while (!existsSync(pidPath) && Date.now() < deadline) await Bun.sleep(25);
  await stopKeeper(tempRoot);
  rmSync(tempRoot, { recursive: true, force: true });
}

function isolatedEnvironment(tempRoot: string): Record<string, string> {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
  ) as Record<string, string>;
  env.TMPDIR = tempRoot;
  env.ROOST_WORKER_DATA_DIR = join(tempRoot, "worker-data");
  env.ROOST_KEEPER_QUIET = "1";
  return env;
}

async function runTestFile(testPath: string): Promise<number> {
  const tempRoot = mkdtempSync(join(tmpdir(), "roost-worker-test-"));
  const relativePath = relative(REPOSITORY_ROOT, testPath);
  const proc = Bun.spawn({
    // Same budget as the unit profile: these files spawn real keepers, PTYs and
    // sqlite work, so Bun's 5 s default fails them as timeouts under contention
    // while proving nothing. CHILD_TIMEOUT_MS below is still the hang backstop.
    cmd: [process.execPath, "test", "--timeout", "30000", relativePath],
    cwd: REPOSITORY_ROOT,
    env: isolatedEnvironment(tempRoot),
    stdio: ["inherit", "inherit", "inherit"],
  });

  let timedOut = false;
  const timeout = setTimeout(() => {
    timedOut = true;
    proc.kill("SIGKILL");
  }, CHILD_TIMEOUT_MS);

  try {
    const exitCode = await proc.exited;
    if (timedOut) {
      console.error(`worker test timed out after ${CHILD_TIMEOUT_MS / 1_000}s: ${relativePath}`);
      return 1;
    }
    return exitCode ?? 1;
  } finally {
    clearTimeout(timeout);
    await cleanupTestRoot(tempRoot);
  }
}

const tests = [...new Bun.Glob("apps/worker/tests/**/*.test.ts").scanSync({ cwd: REPOSITORY_ROOT })]
  .map((path) => join(REPOSITORY_ROOT, path))
  .sort();

if (tests.length === 0) {
  console.error("No worker tests found.");
  process.exit(1);
}

for (const testPath of tests) {
  console.log(`\n==> worker: ${relative(REPOSITORY_ROOT, testPath)}`);
  const exitCode = await runTestFile(testPath);
  if (exitCode !== 0) process.exit(exitCode);
}
