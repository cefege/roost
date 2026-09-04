// Provides bounded command and filesystem checks for SaaS host prerequisites.
// The prerequisite sweep composes these parsers around privileged system queries.
// Each check rejects ambiguous metadata rather than trusting partial command output.
import { lstatSync, statfsSync } from "node:fs";
import type { CommandResult, CommandRunner } from "./docker.ts";
import { ALERT_DISK_RATIO, OUTPUT_LIMIT, STOP_DISK_RATIO } from "./host-config.ts";

export function checkedSecretFile(path: string, name: string): void {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 64 * 1024) {
    throw new Error(`${name} is invalid`);
  }
  if ((stat.mode & 0o077) !== 0) throw new Error(`${name} must not be group/world accessible`);
}

export function diskUsedRatio(path = "/srv"): number {
  const stat = statfsSync(path);
  const blocks = Number(stat.blocks);
  const available = Number(stat.bavail);
  if (!Number.isFinite(blocks) || blocks <= 0 || !Number.isFinite(available) || available < 0) {
    throw new Error("host disk usage is unavailable");
  }
  return 1 - available / blocks;
}

export function assertDisk(ratio: number, onAlert: (message: string) => void): void {
  if (!Number.isFinite(ratio) || ratio < 0 || ratio > 1) throw new Error("host disk usage is invalid");
  if (ratio >= STOP_DISK_RATIO) throw new Error("host disk is at or above 85%; provisioning and rollout are stopped");
  if (ratio >= ALERT_DISK_RATIO) onAlert(`host disk usage is ${Math.floor(ratio * 100)}%`);
}

export async function defaultRunner(argv: readonly string[]): Promise<CommandResult> {
  const program = argv[0];
  if (!program) throw new Error("empty prerequisite command");
  const process = Bun.spawn([...argv], { stdout: "pipe", stderr: "pipe", stdin: "ignore" });
  const [exitCode, stdout, stderr] = await Promise.all([
    process.exited,
    new Response(process.stdout).text(),
    new Response(process.stderr).text(),
  ]);
  if (Buffer.byteLength(stdout) > OUTPUT_LIMIT || Buffer.byteLength(stderr) > OUTPUT_LIMIT) {
    throw new Error("prerequisite command output exceeded its bound");
  }
  return { exitCode, stdout, stderr };
}

export async function checkedCommand(runner: CommandRunner, label: string, argv: readonly string[]): Promise<string> {
  const result = await runner(argv);
  if (result.exitCode !== 0) throw new Error(`${label} prerequisite failed`);
  return result.stdout.trim();
}
interface ServiceAccount {
  uid: number;
  gid: number;
}

export function parseProperties(raw: string, label: string): Map<string, string> {
  const properties = new Map<string, string>();
  for (const line of raw.split(/\r?\n/)) {
    if (!line) continue;
    const separator = line.indexOf("=");
    if (separator <= 0 || properties.has(line.slice(0, separator))) {
      throw new Error(`${label} returned invalid systemd properties`);
    }
    properties.set(line.slice(0, separator), line.slice(separator + 1));
  }
  return properties;
}

export function assertProperties(
  raw: string,
  label: string,
  expected: Readonly<Record<string, string>>,
  execFragment?: string,
): void {
  const properties = parseProperties(raw, label);
  for (const [name, value] of Object.entries(expected)) {
    if (properties.get(name) !== value) throw new Error(`${label} has unsafe ${name}`);
  }
  if (execFragment && !properties.get("ExecStart")?.includes(execFragment)) {
    throw new Error(`${label} has an unsafe ExecStart`);
  }
}

export function parseServiceAccount(raw: string, name: string): ServiceAccount {
  const fields = raw.split(":");
  const uid = Number(fields[2]);
  const gid = Number(fields[3]);
  if (fields.length !== 7
    || fields[0] !== name
    || !Number.isSafeInteger(uid)
    || !Number.isSafeInteger(gid)
    || uid <= 0
    || gid <= 0
    || !/(?:^|\/)(?:false|nologin)$/.test(fields[6] ?? "")) {
    throw new Error(`${name} must be a dedicated non-login service user`);
  }
  return { uid, gid };
}

export function parseServiceGroup(raw: string, name: string, expectedGid: number): void {
  const fields = raw.split(":");
  if (fields.length !== 4 || fields[0] !== name || Number(fields[2]) !== expectedGid) {
    throw new Error(`${name} must have a dedicated service group`);
  }
}

export function assertFileMetadata(raw: string, expected: string, label: string): void {
  if (raw.trim() !== expected) throw new Error(`${label} has unsafe ownership or mode`);
}

export function assertLoopbackListener(raw: string, port: number, label: string): void {
  const lines = raw.trim().split(/\r?\n/).filter(Boolean);
  if (lines.length !== 1 || !new RegExp(`(?:^|\\s)127[.]0[.]0[.]1:${port}(?:\\s|$)`).test(lines[0]!)) {
    throw new Error(`${label} must bind exactly 127.0.0.1:${port}`);
  }
}

export async function assertDirectAccessRejected(
  runner: CommandRunner,
  label: string,
  user: string,
  url: string,
): Promise<void> {
  const result = await runner([
    "runuser", "--user", user, "--",
    "curl", "--silent", "--show-error", "--output", "/dev/null",
    "--connect-timeout", "1", "--max-time", "1", url,
  ]);
  if (result.exitCode === 0) throw new Error(`${label} is directly reachable by ${user}`);
}
