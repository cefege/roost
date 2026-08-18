import { closeSync, existsSync, mkdtempSync, openSync, readFileSync, rmSync } from "node:fs";
import { availableParallelism, tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";

const REPOSITORY_ROOT = dirname(import.meta.dir);
const KEEPER_ENTRY = "multiplexed-main.ts";
const KEEPER_SOCKET = "mux-keeper.sock";
const CHILD_TIMEOUT_MS = 90_000;
const GRACEFUL_STOP_MS = 5_000;
const FORCED_STOP_MS = 2_000;
const KEEPER_START_SETTLE_MS = 1_000;

// Each pooled file is a full stack: a `bun test` process, a real keeper
// subprocess, real PTYs and sqlite work. 4 is the default because the box this
// runs on has 8 cores and a file's own keeper + shells already use more than one
// of them; the cap keeps the pool from oversubscribing and turning I/O waits
// into timeouts. Override with ROOST_WORKER_TEST_JOBS (clamped to 1..cores).
const DEFAULT_JOBS = 4;
const JOB_CEILING = Math.max(1, Math.min(availableParallelism(), 8));

function resolveJobs(): number {
  const raw = process.env.ROOST_WORKER_TEST_JOBS;
  if (raw === undefined || raw.trim() === "") return Math.min(DEFAULT_JOBS, JOB_CEILING);
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    console.error(`ignoring ROOST_WORKER_TEST_JOBS=${raw}: not a positive integer`);
    return Math.min(DEFAULT_JOBS, JOB_CEILING);
  }
  return Math.min(parsed, JOB_CEILING);
}

// Files that MUST own the box alone: not "slow" or "heavy", only files whose
// oracle is global process state, where a sibling file's concurrent keeper,
// keeper child or PTY shell could satisfy/violate an assertion. Empty, and that
// is a measured result, not an assumption. The three candidates that scan the
// whole process table were checked pattern by pattern:
//   - keeper-death-reconcile.test.ts:65 and keeper-stray-reap.test.ts:75 run
//     `pgrep -f SOCK_PATH`, and SOCK_PATH is
//     `$TMPDIR/roost-test-keeper-death-<pid>/mux-keeper.sock` (:21) /
//     `roost-test-stray-reap-<pid>` (:18) — the test process's own pid, so a
//     sibling keeper's argv can never match.
//   - keeper-child-reap.test.ts:35-36 pgreps `sleep 8873310` / `sleep 9982210`;
//     grep of apps/worker/tests shows those literals in that one file only, and
//     the keeper's reap walks a ppid graph rooted at its own channel leader
//     (keeper-process-reap.ts collectProcessTree), never a name pattern, so a
//     sibling's shells are outside the tree it enumerates.
// Empirically: all eight keeper-family files run simultaneously (8-way, more
// contention than the pool ever creates) passed twice, and two full pooled runs
// matched the serial baseline file for file. Re-add an entry here — with the
// pattern that makes it global — only against evidence of a pooled-only failure.
const SERIAL_TEST_FILES: readonly string[] = [];

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
  // The child's output now lands in a file, not our terminal, so Bun would drop
  // colour from the block we replay. Keep it when we are the ones with a TTY.
  if (process.stdout.isTTY) env.FORCE_COLOR = "1";
  return env;
}

interface FileResult {
  file: string;
  exitCode: number;
  output: string;
  ms: number;
}

async function runTestFile(testPath: string, logDir: string, slot: number): Promise<FileResult> {
  const tempRoot = mkdtempSync(join(tmpdir(), "roost-worker-test-"));
  const relativePath = relative(REPOSITORY_ROOT, testPath);
  const started = Date.now();
  // One fd shared by stdout and stderr: the kernel appends both in real order,
  // so the replayed block is byte-identical to what an inherited stdio run
  // printed. Buffering (rather than inheriting) is what keeps four concurrent
  // children from interleaving half-lines under one `==> worker:` header.
  const logPath = join(logDir, `slot-${slot}-${Date.now()}.log`);
  const logFd = openSync(logPath, "w");
  let exitCode: number;
  try {
    const proc = Bun.spawn({
      // Same budget as the unit profile: these files spawn real keepers, PTYs and
      // sqlite work, so Bun's 5 s default fails them as timeouts under contention
      // while proving nothing. CHILD_TIMEOUT_MS below is still the hang backstop.
      cmd: [process.execPath, "test", "--timeout", "30000", relativePath],
      cwd: REPOSITORY_ROOT,
      env: isolatedEnvironment(tempRoot),
      stdio: ["ignore", logFd, logFd],
    });

    let timedOut = false;
    const timeout = setTimeout(() => {
      timedOut = true;
      proc.kill("SIGKILL");
    }, CHILD_TIMEOUT_MS);

    try {
      const childCode = await proc.exited;
      exitCode = timedOut ? 1 : childCode ?? 1;
      if (timedOut) {
        console.error(`worker test timed out after ${CHILD_TIMEOUT_MS / 1_000}s: ${relativePath}`);
      }
    } finally {
      clearTimeout(timeout);
      await cleanupTestRoot(tempRoot);
    }
  } finally {
    closeSync(logFd);
  }

  let output: string;
  try {
    output = readFileSync(logPath, "utf8");
  } catch (e) {
    output = `<runner could not read captured output: ${String(e)}>\n`;
  }
  rmSync(logPath, { force: true });
  return { file: relativePath, exitCode, output, ms: Date.now() - started };
}

const discovered = [...new Bun.Glob("apps/worker/tests/**/*.test.ts").scanSync({ cwd: REPOSITORY_ROOT })].sort();

if (discovered.length === 0) {
  console.error("No worker tests found.");
  process.exit(1);
}

const missingSerial = SERIAL_TEST_FILES.filter((file) => !discovered.includes(file));
if (missingSerial.length > 0) {
  // A renamed/deleted quarantine entry must not silently start running pooled.
  console.error(`SERIAL_TEST_FILES entries no longer exist: ${missingSerial.join(", ")}`);
  process.exit(1);
}

const serialFiles = discovered.filter((file) => SERIAL_TEST_FILES.includes(file));
const pooledFiles = discovered.filter((file) => !SERIAL_TEST_FILES.includes(file));
// Scheduled order == emission order, so a run's transcript is identical file to
// file every time regardless of which slot won the race. Serial files run last,
// with nothing else in flight, which is the exclusivity they were listed for.
const scheduled = [...pooledFiles, ...serialFiles];
const jobs = resolveJobs();
const logDir = mkdtempSync(join(tmpdir(), "roost-worker-test-logs-"));

const results = new Array<FileResult | null>(scheduled.length).fill(null);
let flushed = 0;
function flushCompleted(): void {
  while (flushed < results.length) {
    const result = results[flushed];
    if (result === null) return;
    console.log(`\n==> worker: ${result.file} (${(result.ms / 1_000).toFixed(1)}s${result.exitCode === 0 ? "" : `, exit ${result.exitCode}`})`);
    process.stdout.write(result.output);
    flushed++;
  }
}

const runStarted = Date.now();
console.log(
  `worker tests: ${scheduled.length} files, pool=${jobs} (cap ${JOB_CEILING}), ` +
    `${serialFiles.length} serialized: ${serialFiles.map((f) => relative("apps/worker/tests", f)).join(", ") || "none"}`,
);

let nextPooled = 0;
await Promise.all(
  Array.from({ length: Math.min(jobs, pooledFiles.length) }, async (_unused, slot) => {
    while (nextPooled < pooledFiles.length) {
      const index = nextPooled++;
      results[index] = await runTestFile(join(REPOSITORY_ROOT, pooledFiles[index]), logDir, slot);
      flushCompleted();
    }
  }),
);

for (const [offset, file] of serialFiles.entries()) {
  const index = pooledFiles.length + offset;
  results[index] = await runTestFile(join(REPOSITORY_ROOT, file), logDir, 0);
  flushCompleted();
}

rmSync(logDir, { recursive: true, force: true });

// Every file runs even after one fails (a pooled batch is already in flight when
// the first failure lands), then the run exits with the first failing file's
// code — any failing file still fails the run.
const failures = results.filter((r): r is FileResult => r !== null && r.exitCode !== 0);
const totalSeconds = ((Date.now() - runStarted) / 1_000).toFixed(1);
console.log(
  `\n==> worker summary: ${results.length - failures.length}/${results.length} files passed in ${totalSeconds}s (pool=${jobs})`,
);
for (const failure of failures) console.log(`    FAIL exit=${failure.exitCode} ${failure.file}`);
process.exit(failures.length === 0 ? 0 : failures[0].exitCode);
