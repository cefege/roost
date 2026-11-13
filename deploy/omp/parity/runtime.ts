import { closeSync, openSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";

export interface RunningChild {
  pid: number;
  exited: Promise<number>;
  readonly exitCode: number | null;
  stop(): Promise<void>;
}

export function isolatedEnvironment(home: string, extra: Record<string, string> = {}): Record<string, string> {
  const environment: Record<string, string> = {};
  // The transpiler cache is content-addressed compiled source, not OMP state;
  // sharing it spares every isolated HOME a cold compile of the whole tree.
  for (const key of ["PATH", "LANG", "LC_ALL", "SystemRoot", "WINDIR", "PATHEXT", "PLAYWRIGHT_BROWSERS_PATH", "BUN_RUNTIME_TRANSPILER_CACHE_PATH"]) {
    const value = process.env[key];
    if (value !== undefined) environment[key] = value;
  }
  return {
    ...environment,
    HOME: home,
    USERPROFILE: home,
    TMPDIR: join(home, "tmp"),
    TMP: join(home, "tmp"),
    TEMP: join(home, "tmp"),
    XDG_CONFIG_HOME: join(home, ".config"),
    XDG_CACHE_HOME: join(home, ".cache"),
    XDG_DATA_HOME: join(home, ".local", "share"),
    XDG_STATE_HOME: join(home, ".local", "state"),
    PI_CODING_AGENT_DIR: join(home, ".omp", "agent"),
    PI_CONFIG_DIR: ".omp",
    AWS_EC2_METADATA_DISABLED: "true",
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
    TZ: "UTC",
    ...extra,
  };
}

export async function createHome(home: string) {
  await Promise.all(["tmp", ".config", ".cache", ".local/share", ".local/state", ".omp/agent"].map(
    directory => mkdir(join(home, directory), { recursive: true }),
  ));
}

export async function within<T>(promise: Promise<T>, label: string, timeout = 30_000): Promise<T> {
  const deadline = Promise.withResolvers<never>();
  const timer = setTimeout(() => deadline.reject(new Error(`${label}: exceeded ${timeout}ms`)), timeout);
  try {
    return await Promise.race([promise, deadline.promise]);
  } finally {
    clearTimeout(timer);
  }
}

export async function eventually<T>(label: string, observe: () => Promise<T | undefined>, timeout = 15_000): Promise<T> {
  const deadline = Date.now() + timeout;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      const value = await observe();
      if (value !== undefined) return value;
    } catch (error) {
      lastError = error;
    }
    await Bun.sleep(25);
  }
  throw new Error(`${label}: no matching observation within ${timeout}ms`, { cause: lastError });
}

export async function startLogged(
  command: string[],
  cwd: string,
  env: Record<string, string>,
  logPath: string,
): Promise<RunningChild> {
  await mkdir(resolve(logPath, ".."), { recursive: true });
  const log = openSync(logPath, "a");
  const child = (() => {
    try {
      return Bun.spawn(command, { cwd, env, stdin: "ignore", stdout: log, stderr: log, detached: true });
    } finally {
      closeSync(log);
    }
  })();
  return {
    pid: child.pid,
    exited: child.exited,
    get exitCode() { return child.exitCode; },
    async stop() {
      if (child.exitCode !== null) return;
      try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill("SIGTERM"); }
      try {
        await within(child.exited, "fixture process shutdown", 5_000);
      } catch {
        try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
        await child.exited;
      }
    },
  };
}

export async function execute(command: string[], cwd: string, env: Record<string, string>, timeoutMs = 60_000) {
  const child = Bun.spawn(command, { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe", detached: true });
  let result: [number, string, string];
  try {
    result = await within(Promise.all([
      child.exited,
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
    ]), command.join(" "), timeoutMs);
  } catch (error) {
    try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
    await child.exited;
    throw error;
  }
  const [code, stdout, stderr] = result;
  if (code !== 0) throw new Error(`${command[0]} exited ${code}: ${stderr}`);
  return stdout;
}

export function freeLoopbackPort() {
  const reservation = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response(null, { status: 404 }) });
  const port = reservation.port;
  reservation.stop(true);
  if (port === undefined) throw new Error("Loopback listener did not allocate a port");
  return port;
}

export function objectValue(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label}: expected object`);
  return value as Record<string, unknown>;
}

export function stringValue(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label}: expected string`);
  return value;
}
