import { randomBytes } from "node:crypto";
import { dirname, win32 } from "node:path";
import { createConnection, type Socket } from "node:net";
import { supportedHostPlatform } from "./platform.ts";

const MAX_HELPER_OUTPUT_BYTES = 4 * 1024 * 1024;
const DEFAULT_HELPER_TIMEOUT_MS = 15_000;

export type WindowsHelperOperation =
  | "version"
  | "flush-file"
  | "replace-file"
  | "remove-file"
  | "apply-dacl"
  | "get-dacl"
  | "apply-sddl"
  | "probe-exclusive-open"
  | "current-user-sid"
  | "host-sample"
  | "process-snapshot"
  | "listening-ports"
  | "verify-cms-detached"
  | "verify-authenticode"
  | "extract-zip"
  | "assert-service-context"
  | "resolve-account-sid"
  | "grant-logon-as-service"
  | "apply-service-dacl"
  | "configure-service-account"
  | "service-query"
  | "service-config"
  | "service-start"
  | "service-stop"
  | "job-host";

export interface RunWindowsHelperOptions {
  helperPath?: string;
  timeoutMs?: number;
  env?: Record<string, string | undefined>;
  /** One binary stdin payload. The helper receives uint32-LE length followed by these bytes. */
  input?: Uint8Array;
  /** Zero both the framed copy and the caller-owned input once the helper has exited. */
  sensitive?: boolean;
  /** Test seam for exercising the argv/result contract on a non-Windows runner. */
  allowNonWindows?: boolean;
}

export function resolveWindowsHelperPath(env: Record<string, string | undefined> = process.env): string {
  if (env.ROOST_WIN_HELPER) return env.ROOST_WIN_HELPER;
  return win32.join(dirname(process.execPath), "roost-win-helper.exe");
}

function helperArgv(operation: WindowsHelperOperation, args: readonly string[], options: RunWindowsHelperOptions): string[] {
  if (!options.allowNonWindows && supportedHostPlatform() !== "win32") {
    throw new Error(`Windows helper operation ${operation} requested on ${process.platform}`);
  }
  return [options.helperPath ?? resolveWindowsHelperPath(options.env), operation, ...args];
}

function parseHelperResult<T>(operation: WindowsHelperOperation, stdout: string, stderr: string, exitCode: number): T {
  if (exitCode !== 0) {
    let detail = stderr.trim();
    try {
      const parsed = JSON.parse(stdout) as { error?: string };
      detail = parsed.error ?? detail;
    } catch { /* retain stderr */ }
    throw new Error(`roost-win-helper ${operation} failed (${exitCode}): ${detail || "no detail"}`);
  }
  try {
    return JSON.parse(stdout) as T;
  } catch (error) {
    throw new Error(`roost-win-helper ${operation} returned invalid JSON: ${String(error)}`);
  }
}

function frameHelperInput(input: Uint8Array | undefined): Uint8Array | undefined {
  if (!input) return undefined;
  if (input.byteLength > 0xffff_ffff) throw new Error("roost-win-helper input exceeds uint32 framing limit");
  const framed = new Uint8Array(input.byteLength + 4);
  new DataView(framed.buffer, framed.byteOffset, 4).setUint32(0, input.byteLength, true);
  framed.set(input, 4);
  return framed;
}

export async function runWindowsHelper<T>(
  operation: WindowsHelperOperation,
  args: readonly string[] = [],
  options: RunWindowsHelperOptions = {},
): Promise<T> {
  let framedInput: Uint8Array | undefined;
  try {
    framedInput = frameHelperInput(options.input);
    const proc = Bun.spawn(helperArgv(operation, args, options), {
      stdin: framedInput ?? "ignore",
      stdout: "pipe",
      stderr: "pipe",
      env: options.env ? { ...process.env, ...options.env } : process.env,
      windowsHide: true,
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      try { proc.kill(); } catch { /* already exited */ }
    }, options.timeoutMs ?? DEFAULT_HELPER_TIMEOUT_MS);
    try {
      const [stdoutBytes, stderrBytes, exitCode] = await Promise.all([
        new Response(proc.stdout).arrayBuffer(),
        new Response(proc.stderr).arrayBuffer(),
        proc.exited,
      ]);
      if (timedOut) throw new Error(`roost-win-helper ${operation} timed out`);
      if (stdoutBytes.byteLength > MAX_HELPER_OUTPUT_BYTES || stderrBytes.byteLength > MAX_HELPER_OUTPUT_BYTES) {
        throw new Error(`roost-win-helper ${operation} exceeded output limit`);
      }
      const decoder = new TextDecoder();
      return parseHelperResult<T>(operation, decoder.decode(stdoutBytes), decoder.decode(stderrBytes), exitCode);
    } finally {
      clearTimeout(timer);
    }
  } finally {
    if (options.sensitive) {
      framedInput?.fill(0);
      options.input?.fill(0);
    }
  }
}

export function runWindowsHelperSync<T>(
  operation: WindowsHelperOperation,
  args: readonly string[] = [],
  options: RunWindowsHelperOptions = {},
): T {
  let framedInput: Uint8Array | undefined;
  try {
    framedInput = frameHelperInput(options.input);
    const result = Bun.spawnSync(helperArgv(operation, args, options), {
      stdin: framedInput,
      stdout: "pipe",
      stderr: "pipe",
      env: options.env ? { ...process.env, ...options.env } : process.env,
      timeout: options.timeoutMs ?? DEFAULT_HELPER_TIMEOUT_MS,
      windowsHide: true,
    });
    if (result.stdout.byteLength > MAX_HELPER_OUTPUT_BYTES || result.stderr.byteLength > MAX_HELPER_OUTPUT_BYTES) {
      throw new Error(`roost-win-helper ${operation} exceeded output limit`);
    }
    return parseHelperResult<T>(operation, result.stdout.toString(), result.stderr.toString(), result.exitCode);
  } finally {
    if (options.sensitive) {
      framedInput?.fill(0);
      options.input?.fill(0);
    }
  }
}

export interface WindowsHelperVersion {
  protocol: number;
  helper: "roost-win-helper";
  arch: "x64";
  commands: WindowsHelperOperation[];
}

export async function windowsHelperVersion(options: RunWindowsHelperOptions = {}): Promise<WindowsHelperVersion> {
  const result = await runWindowsHelper<WindowsHelperVersion>("version", [], options);
  if (result.protocol !== 1 || result.helper !== "roost-win-helper" || result.arch !== "x64" || !Array.isArray(result.commands)) {
    throw new Error("incompatible roost-win-helper protocol");
  }
  return result;
}

export async function windowsFlushFile(path: string, options: RunWindowsHelperOptions = {}): Promise<void> {
  await runWindowsHelper<{ ok: true }>("flush-file", [path], options);
}

export async function windowsReplaceFile(source: string, destination: string, options: RunWindowsHelperOptions = {}): Promise<void> {
  await runWindowsHelper<{ ok: true }>("replace-file", [source, destination], options);
}

export async function windowsRemoveFile(path: string, options: RunWindowsHelperOptions = {}): Promise<void> {
  await runWindowsHelper<{ ok: true }>("remove-file", [path], options);
}

export async function windowsApplyPrivateDacl(path: string, options: RunWindowsHelperOptions = {}): Promise<void> {
  await runWindowsHelper<{ ok: true }>("apply-dacl", [path], options);
}

export async function windowsCurrentUserSid(options: RunWindowsHelperOptions = {}): Promise<string> {
  const result = await runWindowsHelper<{ sid: string }>("current-user-sid", [], options);
  if (!/^S-1-(?:\d+-)+\d+$/.test(result.sid)) throw new Error("invalid SID from roost-win-helper");
  return result.sid;
}

export interface WindowsHostSample {
  cpu_pct: number;
  mem_used_bytes: number;
  mem_total_bytes: number;
  disk_used_bytes: number;
  disk_total_bytes: number;
  net: { rxBytes: number; txBytes: number };
}

export interface WindowsProcessRecord {
  pid: number;
  ppid: number;
  pgid: number;
  tpgid: number;
  comm: string;
  args: string;
}

export interface WindowsListeningPort {
  pid: number;
  address: string;
  port: number;
}

function finiteNonnegative(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`invalid ${field} from roost-win-helper`);
  }
  return value;
}

export async function windowsHostSample(options: RunWindowsHelperOptions = {}): Promise<WindowsHostSample> {
  const raw = await runWindowsHelper<WindowsHostSample>("host-sample", [], options);
  return {
    cpu_pct: finiteNonnegative(raw.cpu_pct, "cpu_pct"),
    mem_used_bytes: finiteNonnegative(raw.mem_used_bytes, "mem_used_bytes"),
    mem_total_bytes: finiteNonnegative(raw.mem_total_bytes, "mem_total_bytes"),
    disk_used_bytes: finiteNonnegative(raw.disk_used_bytes, "disk_used_bytes"),
    disk_total_bytes: finiteNonnegative(raw.disk_total_bytes, "disk_total_bytes"),
    net: {
      rxBytes: finiteNonnegative(raw.net?.rxBytes, "net.rxBytes"),
      txBytes: finiteNonnegative(raw.net?.txBytes, "net.txBytes"),
    },
  };
}

export async function windowsProcessSnapshot(options: RunWindowsHelperOptions = {}): Promise<WindowsProcessRecord[]> {
  const records = await runWindowsHelper<WindowsProcessRecord[]>("process-snapshot", [], options);
  if (!Array.isArray(records)) throw new Error("invalid process snapshot from roost-win-helper");
  return records.map((record) => {
    if (!Number.isInteger(record.pid) || record.pid <= 0 || !Number.isInteger(record.ppid) || record.ppid < 0) {
      throw new Error("invalid process identity from roost-win-helper");
    }
    return {
      pid: record.pid,
      ppid: record.ppid,
      pgid: Number.isInteger(record.pgid) ? record.pgid : record.pid,
      tpgid: Number.isInteger(record.tpgid) ? record.tpgid : record.pid,
      comm: String(record.comm),
      args: String(record.args),
    };
  });
}

export async function windowsListeningPorts(options: RunWindowsHelperOptions = {}): Promise<WindowsListeningPort[]> {
  const records = await runWindowsHelper<WindowsListeningPort[]>("listening-ports", [], options);
  if (!Array.isArray(records)) throw new Error("invalid listening port list from roost-win-helper");
  return records.map((record) => {
    if (!Number.isInteger(record.pid) || record.pid <= 0 || !Number.isInteger(record.port) || record.port <= 0 || record.port > 65_535) {
      throw new Error("invalid listening port record from roost-win-helper");
    }
    return { pid: record.pid, address: String(record.address), port: record.port };
  });
}

export async function windowsProbeExclusiveOpen(path: string, options: RunWindowsHelperOptions = {}): Promise<boolean> {
  const result = await runWindowsHelper<{ exclusive: boolean }>("probe-exclusive-open", [path], options);
  return result.exclusive === true;
}

export async function windowsReadDacl(path: string, options: RunWindowsHelperOptions = {}): Promise<{ sddl: string }> {
  const result = await runWindowsHelper<{ sddl: string }>("get-dacl", [path], options);
  if (!result.sddl || typeof result.sddl !== "string") throw new Error("invalid DACL result from roost-win-helper");
  return result;
}

export async function windowsApplySddl(path: string, sddl: string, options: RunWindowsHelperOptions = {}): Promise<void> {
  await runWindowsHelper<{ ok: true }>("apply-sddl", [path, sddl], options);
}

export interface WindowsSignatureVerification {
  valid: boolean;
  publisherSha256: string;
  timestamped: boolean;
}

function validateSignatureVerification(result: WindowsSignatureVerification): WindowsSignatureVerification {
  if (result.valid !== true || !/^[a-fA-F0-9]{64}$/.test(result.publisherSha256) || typeof result.timestamped !== "boolean") {
    throw new Error("Windows signature verification failed closed");
  }
  return {
    valid: true,
    publisherSha256: result.publisherSha256.toLowerCase(),
    timestamped: result.timestamped,
  };
}

export async function windowsVerifyDetachedCms(
  manifestPath: string,
  signaturePath: string,
  publisherSha256: string,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsSignatureVerification> {
  const result = await runWindowsHelper<WindowsSignatureVerification>(
    "verify-cms-detached",
    [manifestPath, signaturePath, "--publisher-sha256", publisherSha256],
    options,
  );
  return validateSignatureVerification(result);
}

export async function windowsVerifyAuthenticode(
  assetPath: string,
  publisherSha256: string,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsSignatureVerification> {
  const result = await runWindowsHelper<WindowsSignatureVerification>(
    "verify-authenticode",
    [assetPath, "--publisher-sha256", publisherSha256],
    options,
  );
  return validateSignatureVerification(result);
}

export interface WindowsZipManifestEntry {
  path: string;
  size: number;
  sha256: string;
}

export interface WindowsZipManifest {
  files: WindowsZipManifestEntry[];
}

export interface WindowsZipExtraction {
  files: string[];
}

export async function windowsExtractZip(
  zipPath: string,
  destinationPath: string,
  manifest: WindowsZipManifest,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsZipExtraction> {
  const input = new TextEncoder().encode(JSON.stringify(manifest));
  const result = await runWindowsHelper<WindowsZipExtraction>(
    "extract-zip",
    [zipPath, destinationPath],
    { ...options, input },
  );
  if (!Array.isArray(result.files) || result.files.some((path) => typeof path !== "string")) {
    throw new Error("invalid ZIP extraction result from roost-win-helper");
  }
  return result;
}

export type RoostWindowsServiceName =
  | "RoostKeeperV2"
  | "RoostWorkerV2"
  | "RoostCoordinatorV2"
  | "RoostUpdaterV2";

export interface WindowsServiceContext {
  service: "RoostUpdaterV2";
  pid: number;
  isolatedFromWorkerCoordinatorJobs: true;
}

export async function windowsAssertUpdaterServiceContext(
  pid: number,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsServiceContext> {
  const result = await runWindowsHelper<WindowsServiceContext>(
    "assert-service-context",
    ["RoostUpdaterV2", String(pid)],
    options,
  );
  if (result.service !== "RoostUpdaterV2" || result.pid !== pid || result.isolatedFromWorkerCoordinatorJobs !== true) {
    throw new Error("invalid updater service context from roost-win-helper");
  }
  return result;
}

export interface WindowsResolvedAccount {
  sid: string;
  canonicalAccount: string;
}

export async function windowsResolveAccountSid(
  account: string,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsResolvedAccount> {
  const result = await runWindowsHelper<WindowsResolvedAccount>("resolve-account-sid", [account], options);
  if (!/^S-1-(?:\d+-)+\d+$/.test(result.sid) || !result.canonicalAccount) {
    throw new Error("invalid resolved account from roost-win-helper");
  }
  return result;
}

export async function windowsGrantLogonAsService(
  sid: string,
  options: RunWindowsHelperOptions = {},
): Promise<{ changed: boolean }> {
  return runWindowsHelper<{ changed: boolean }>("grant-logon-as-service", [sid], options);
}

const ROOST_SERVICE_CONTROL_GRANT = "START,STOP,QUERY_STATUS,QUERY_CONFIG,CHANGE_CONFIG";

export async function windowsApplyServiceDacl(
  service: RoostWindowsServiceName,
  sid: string,
  options: RunWindowsHelperOptions = {},
): Promise<{ sddl: string }> {
  return runWindowsHelper<{ sddl: string }>(
    "apply-service-dacl",
    [service, sid, ROOST_SERVICE_CONTROL_GRANT],
    options,
  );
}

export async function windowsConfigureServiceAccount(
  service: RoostWindowsServiceName,
  canonicalAccount: string,
  passwordUtf8: Uint8Array,
  options: RunWindowsHelperOptions = {},
): Promise<{ configured: boolean }> {
  return runWindowsHelper<{ configured: boolean }>(
    "configure-service-account",
    [service, canonicalAccount],
    { ...options, input: passwordUtf8, sensitive: true },
  );
}

export type WindowsServiceState =
  | "stopped"
  | "start-pending"
  | "stop-pending"
  | "running"
  | "continue-pending"
  | "pause-pending"
  | "paused";

export type WindowsServiceRecoveryActionType =
  | "none"
  | "restart"
  | "reboot"
  | "run-command";

export interface WindowsServiceRecoveryAction {
  type: WindowsServiceRecoveryActionType;
  delayMs: number;
}

export interface WindowsServiceRecoveryPolicy {
  resetPeriodSeconds: number;
  rebootMessage: string;
  command: string;
  actions: WindowsServiceRecoveryAction[];
  actionsOnNonCrashFailures: boolean;
}

export interface WindowsServiceSnapshot {
  name: RoostWindowsServiceName;
  state: WindowsServiceState;
  pid: number;
  startType: "automatic" | "manual" | "disabled";
  imagePathRaw: string;
  binaryArgv: string[];
  account: string;
  dependencies: string[];
  environment: Record<string, string>;
  displayName: string;
  description: string;
  recoveryPolicy: WindowsServiceRecoveryPolicy;
  securityDescriptor: string;
}

export interface WindowsServiceConfig {
  binaryArgv?: string[];
  startType?: "automatic" | "manual" | "disabled";
  dependencies?: string[];
  environment?: Record<string, string>;
  displayName?: string;
  description?: string;
  recoveryPolicy?: WindowsServiceRecoveryPolicy;
  securityDescriptor?: string;
}

export interface WindowsServiceStateResult {
  name: RoostWindowsServiceName;
  state: WindowsServiceState;
  pid: number;
}

export async function windowsQueryService(
  service: RoostWindowsServiceName,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsServiceSnapshot> {
  return runWindowsHelper<WindowsServiceSnapshot>("service-query", [service], options);
}

export async function windowsConfigureService(
  service: RoostWindowsServiceName,
  config: WindowsServiceConfig,
  options: RunWindowsHelperOptions = {},
): Promise<{ configured: boolean }> {
  const input = new TextEncoder().encode(JSON.stringify(config));
  return runWindowsHelper<{ configured: boolean }>("service-config", [service], { ...options, input });
}

export async function windowsStartService(
  service: RoostWindowsServiceName,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsServiceStateResult> {
  return runWindowsHelper<WindowsServiceStateResult>("service-start", [service], options);
}

export async function windowsStopService(
  service: RoostWindowsServiceName,
  timeoutMs: number,
  options: RunWindowsHelperOptions = {},
): Promise<WindowsServiceStateResult> {
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) throw new Error("service stop timeout must be a positive integer");
  return runWindowsHelper<WindowsServiceStateResult>(
    "service-stop",
    [service, "--timeout-ms", String(timeoutMs)],
    { ...options, timeoutMs: Math.max(options.timeoutMs ?? 0, timeoutMs + 1_000) },
  );
}

export interface WindowsJobHostSpec {
  terminal: Bun.Terminal;
  executable: string;
  argv: readonly string[];
  cwd: string;
  env: Record<string, string>;
  helperPath?: string;
  timeoutMs?: number;
}

export interface WindowsJobHostHandle {
  process: Bun.Subprocess;
  controlPipe: string;
  assignedPid: number;
  close(): Promise<void>;
  closed: Promise<{ exitCode: number | null }>;
}

function appendField(parts: Buffer[], value: string): void {
  const bytes = Buffer.from(value, "utf8");
  const length = Buffer.allocUnsafe(4);
  length.writeUInt32LE(bytes.byteLength, 0);
  parts.push(length, bytes);
}

function encodeJobHostRequest(capability: string, spec: WindowsJobHostSpec): Buffer {
  const parts: Buffer[] = [Buffer.from("RJH1")];
  appendField(parts, capability);
  appendField(parts, spec.executable);
  appendField(parts, spec.cwd);
  const argc = Buffer.allocUnsafe(4);
  argc.writeUInt32LE(spec.argv.length, 0);
  parts.push(argc);
  for (const arg of spec.argv) appendField(parts, arg);
  const entries = Object.entries(spec.env);
  const envc = Buffer.allocUnsafe(4);
  envc.writeUInt32LE(entries.length, 0);
  parts.push(envc);
  for (const [key, value] of entries) {
    appendField(parts, key);
    appendField(parts, value);
  }
  const payload = Buffer.concat(parts);
  const frame = Buffer.allocUnsafe(payload.byteLength + 4);
  frame.writeUInt32LE(payload.byteLength, 0);
  payload.copy(frame, 4);
  return frame;
}

function encodeJobHostClose(): Buffer {
  const frame = Buffer.allocUnsafe(5);
  frame.writeUInt32LE(1, 0);
  frame[4] = 4;
  return frame;
}

async function connectJobPipe(address: string, deadline: number): Promise<Socket> {
  for (;;) {
    try {
      return await new Promise<Socket>((resolve, reject) => {
        const socket = createConnection(address);
        socket.once("connect", () => resolve(socket));
        socket.once("error", reject);
      });
    } catch (error) {
      if (Date.now() >= deadline) throw new Error(`job-host control pipe did not become ready: ${String(error)}`);
      await Bun.sleep(10);
    }
  }
}

function readJobHostMessages(socket: Socket): {
  assigned: Promise<number>;
  closed: Promise<{ exitCode: number | null }>;
} {
  let buffer = Buffer.alloc(0);
  let assignedResolve!: (pid: number) => void;
  let assignedReject!: (error: Error) => void;
  let closedResolve!: (value: { exitCode: number | null }) => void;
  let closedReject!: (error: Error) => void;
  let assignedDone = false;
  let closedDone = false;
  const assigned = new Promise<number>((resolve, reject) => { assignedResolve = resolve; assignedReject = reject; });
  const closed = new Promise<{ exitCode: number | null }>((resolve, reject) => { closedResolve = resolve; closedReject = reject; });
  const fail = (error: Error) => {
    if (!assignedDone) { assignedDone = true; assignedReject(error); }
    if (!closedDone) { closedDone = true; closedReject(error); }
  };
  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.byteLength >= 4) {
      const length = buffer.readUInt32LE(0);
      if (length > 64 * 1024) return fail(new Error("oversized job-host control frame"));
      if (buffer.byteLength < length + 4) return;
      const message = buffer.subarray(4, 4 + length);
      buffer = buffer.subarray(4 + length);
      const tag = message[0];
      if (tag === 1 && message.byteLength === 5) {
        if (!assignedDone) { assignedDone = true; assignedResolve(message.readUInt32LE(1)); }
      } else if (tag === 2 && message.byteLength === 5) {
        const raw = message.readInt32LE(1);
        if (!closedDone) { closedDone = true; closedResolve({ exitCode: raw < 0 ? null : raw }); }
      } else if (tag === 3) {
        fail(new Error(`job-host: ${message.subarray(1).toString("utf8")}`));
      } else {
        fail(new Error("invalid job-host control frame"));
      }
    }
  });
  socket.once("error", (error) => fail(error));
  socket.once("close", () => {
    if (!closedDone) fail(new Error("job-host control pipe closed without terminal status"));
  });
  return { assigned, closed };
}

/** Spawn the helper under Bun's ConPTY, then deliver shell state over its authenticated control pipe. */
export async function spawnWindowsJobHost(spec: WindowsJobHostSpec): Promise<WindowsJobHostHandle> {
  if (supportedHostPlatform() !== "win32") throw new Error("job-host is available only on Windows");
  const capability = randomBytes(32).toString("hex");
  const controlPipe = `\\\\.\\pipe\\roost-job-${process.pid}-${randomBytes(16).toString("hex")}`;
  const helperPath = spec.helperPath ?? resolveWindowsHelperPath();
  const proc = Bun.spawn([helperPath, "job-host", "--pipe", controlPipe, "--cap", capability], {
    terminal: spec.terminal,
    windowsHide: true,
  });
  const deadline = Date.now() + (spec.timeoutMs ?? 5_000);
  let socket: Socket;
  try {
    socket = await connectJobPipe(controlPipe, deadline);
  } catch (error) {
    try { proc.kill(); } catch { /* already exited */ }
    await proc.exited;
    throw error;
  }
  const status = readJobHostMessages(socket);
  socket.write(encodeJobHostRequest(capability, spec));
  let assignedPid: number;
  try {
    assignedPid = await Promise.race([
      status.assigned,
      Bun.sleep(Math.max(1, deadline - Date.now())).then(() => { throw new Error("job-host assignment timed out"); }),
    ]);
  } catch (error) {
    socket.destroy();
    try { proc.kill(); } catch { /* already exited */ }
    await proc.exited;
    throw error;
  }
  let closeStarted: Promise<void> | undefined;
  return {
    process: proc,
    controlPipe,
    assignedPid,
    closed: status.closed,
    close() {
      if (!closeStarted) {
        closeStarted = (async () => {
          try {
            await new Promise<void>((resolve, reject) => {
              socket.write(encodeJobHostClose(), (error) => error ? reject(error) : resolve());
            });
            await status.closed;
          } finally {
            socket.end();
            await proc.exited;
          }
        })();
      }
      return closeStarted;
    },
  };
}
