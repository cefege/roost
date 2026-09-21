// Installed POSIX worker service parsing and runtime admission boundaries.
// deploy-plist-env.ts re-exports the environment parsers for existing callers.
// Source deploys consume the exact executable named by a valid source-mode unit.

import { existsSync } from "node:fs";
import { posix } from "node:path";
import { posixShellQuote } from "@roost/shared/shell-quote";
import type { WorkerUpdateFailure } from "@roost/shared/worker-update-operation";
import { DeployFailure } from "./deploy-exec.ts";

export type WorkerServicePlatform = "darwin" | "linux";
export type WorkerServiceExecutionMode = "source" | "binary";

export interface WorkerServiceRuntime {
  executable: string;
  mode: WorkerServiceExecutionMode;
}

export interface WorkerRuntimeObservation extends WorkerServiceRuntime {
  bunAbi: string;
  platform: WorkerServicePlatform;
  arch: string;
}

interface WorkerRuntimeCommandResult {
  exit: number;
  stdout: string;
  stderr: string;
}


function unescapeXml(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

function unescapeSystemd(value: string): string {
  return value.replaceAll("%%", "%").replace(/\\([\\\"nrt])/g, (_match, escaped: string) => {
    switch (escaped) {
      case "n": return "\n";
      case "r": return "\r";
      case "t": return "\t";
      default: return escaped;
    }
  });
}

function parsePlistEnvironment(plistText: string): Record<string, string> {
  const environment: Record<string, string> = {};
  const pairs = /<key>(ROOST_[A-Z_]+|GIT_SHA)<\/key>\s*<string>([^<]*)<\/string>/g;
  let pair: RegExpExecArray | null;
  while ((pair = pairs.exec(plistText)) !== null) {
    environment[pair[1]] = unescapeXml(pair[2]);
  }
  return environment;
}

function parseUnitEnvironment(unitText: string): Record<string, string> {
  const environment: Record<string, string> = {};
  const entries = /^Environment=(?:"(ROOST_[A-Z_]+|GIT_SHA)=((?:\\.|[^"])*)"|(ROOST_[A-Z_]+|GIT_SHA)=([^\r\n]*))$/gm;
  let entry: RegExpExecArray | null;
  while ((entry = entries.exec(unitText)) !== null) {
    const quoted = entry[1] !== undefined;
    environment[(quoted ? entry[1] : entry[3])!] = quoted
      ? unescapeSystemd(entry[2]!)
      : unescapeSystemd(entry[4]!.trim());
  }
  return environment;
}

/** Parses installer-emitted service environment values without merging them
 * into process.env. Environment values remain independent per deploy target. */
export function parsePosixServiceEnvironment(
  definition: string,
  platform: WorkerServicePlatform,
): Record<string, string> {
  return platform === "linux" ? parseUnitEnvironment(definition) : parsePlistEnvironment(definition);
}

/** Reads one simple systemd directive from an installed worker unit. */
export function parseSystemdServiceDirective(
  definition: string,
  name: string,
): string | undefined {
  if (!/^[A-Za-z][A-Za-z0-9]*$/.test(name)) return undefined;
  const directive = new RegExp(`^${name}=([^\\r\\n]+)$`, "gm");
  let value: string | undefined;
  let entry: RegExpExecArray | null;
  while ((entry = directive.exec(definition)) !== null) value = entry[1]?.trim();
  if (!value) return undefined;
  return value.startsWith("\"") && value.endsWith("\"") ? value.slice(1, -1) : value;
}

function parsePlistProgramArguments(definition: string): string[] | null {
  const entries = [...definition.matchAll(/<key>ProgramArguments<\/key>\s*<array>([\s\S]*?)<\/array>/g)];
  if (entries.length !== 1) return null;
  const body = entries[0]![1]!;
  const rawArguments = [...body.matchAll(/<string>([^<]*)<\/string>/g)];
  if (rawArguments.length === 0 || body.replace(/<string>[^<]*<\/string>/g, "").trim() !== "") {
    return null;
  }
  return rawArguments.map(argument => unescapeXml(argument[1]!));
}

function parseSystemdCommandArguments(value: string): string[] | null {
  const argumentsList: string[] = [];
  let current = "";
  let quoted = false;
  let tokenStarted = false;
  for (let index = 0; index < value.length; index += 1) {
    const character = value[index]!;
    if (character === "\"") {
      quoted = !quoted;
      tokenStarted = true;
      continue;
    }
    if (character === "\\") {
      const escaped = value[index + 1];
      if (escaped === undefined) return null;
      if (escaped === "n") current += "\n";
      else if (escaped === "r") current += "\r";
      else if (escaped === "t") current += "\t";
      else if (escaped === "\\" || escaped === "\"") current += escaped;
      else return null;
      index += 1;
      tokenStarted = true;
      continue;
    }
    if (character === "%") {
      if (value[index + 1] !== "%") return null;
      current += "%";
      index += 1;
      tokenStarted = true;
      continue;
    }
    if (!quoted && /[ \t]/.test(character)) {
      if (tokenStarted) {
        argumentsList.push(current);
        current = "";
        tokenStarted = false;
      }
      continue;
    }
    current += character;
    tokenStarted = true;
  }
  if (quoted || !tokenStarted) return null;
  argumentsList.push(current);
  return argumentsList;
}

function parseUnitProgramArguments(definition: string): string[] | null {
  const entries = [...definition.matchAll(/^ExecStart=([^\r\n]*)$/gm)];
  if (entries.length !== 1) return null;
  return parseSystemdCommandArguments(entries[0]![1]!.trim());
}

function isCanonicalAbsoluteExecutable(path: string): boolean {
  return posix.isAbsolute(path)
    && path !== "/"
    && path === posix.normalize(path)
    && !/[\r\n\0]/.test(path);
}


/** Parses only the command shape written by the worker installer. A malformed,
 * shell-shaped, or noncanonical executable is intentionally not a runtime. */
export function parseWorkerServiceRuntime(
  definition: string,
  platform: WorkerServicePlatform,
): WorkerServiceRuntime | null {
  const argumentsList = platform === "linux"
    ? parseUnitProgramArguments(definition)
    : parsePlistProgramArguments(definition);
  if (argumentsList?.length !== 2 || !isCanonicalAbsoluteExecutable(argumentsList[0]!)) {
    return null;
  }
  if (isCanonicalAbsoluteExecutable(argumentsList[1]!)
    && argumentsList[1]!.endsWith("/apps/worker/src/main.ts")) {
    return { executable: argumentsList[0]!, mode: "source" };
  }
  if (argumentsList[1] === "worker") {
    return { executable: argumentsList[0]!, mode: "binary" };
  }
  return null;
}

function runtimeUnavailableFailure(message: string): DeployFailure {
  const workerUpdateFailure: WorkerUpdateFailure = {
    code: "runtime_unavailable",
    phase: "preflight",
    message,
    journal: null,
    expectedKeeper: null,
    observedKeeper: null,
    targetContract: null,
  };
  return new DeployFailure(3, message, workerUpdateFailure);
}

/** Turns a service definition parse failure into the typed update boundary.
 * Callers must separately verify this executable exists and is executable. */
export function requireWorkerServiceRuntime(
  definition: string,
  platform: WorkerServicePlatform,
): WorkerServiceRuntime {
  const runtime = parseWorkerServiceRuntime(definition, platform);
  if (runtime === null) {
    throw runtimeUnavailableFailure("installed worker service runtime is unavailable");
  }
  return runtime;
}

/** Source deploys may reuse only a Bun executable the installed source service
 * already proves. A compiled worker remains on its explicit binary update path. */
export function requireSourceWorkerServiceRuntime(
  definition: string,
  platform: WorkerServicePlatform,
): WorkerServiceRuntime {
  const runtime = requireWorkerServiceRuntime(definition, platform);
  if (runtime.mode !== "source") {
    throw runtimeUnavailableFailure("installed worker service uses binary execution mode");
  }
  return runtime;
}

function parseRuntimeObservation(
  executable: string,
  mode: WorkerServiceExecutionMode,
  serialized: string,
  expectedPlatform: WorkerServicePlatform,
): WorkerRuntimeObservation {
  let value: unknown;
  try {
    value = JSON.parse(serialized);
  } catch {
    throw runtimeUnavailableFailure("installed worker executable returned malformed runtime metadata");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw runtimeUnavailableFailure("installed worker executable returned malformed runtime metadata");
  }
  const fields = value as Record<string, unknown>;
  if (typeof fields.bunAbi !== "string" || fields.bunAbi.length === 0
    || fields.platform !== expectedPlatform
    || typeof fields.arch !== "string" || fields.arch.length === 0) {
    throw runtimeUnavailableFailure("installed worker executable runtime metadata is unsupported");
  }
  return {
    executable,
    mode,
    bunAbi: fields.bunAbi,
    platform: expectedPlatform,
    arch: fields.arch,
  };
}

const RUNTIME_PROBE_PROGRAM =
  "process.stdout.write(JSON.stringify({bunAbi:Bun.version,platform:process.platform,arch:process.arch}))";

/** Pins one remote Bun executable for every source deployment command. */
export async function resolveRemoteWorkerRuntime(
  definition: string | null,
  platform: WorkerServicePlatform,
  execute: (command: string) => Promise<WorkerRuntimeCommandResult>,
): Promise<WorkerRuntimeObservation> {
  let runtime: WorkerServiceRuntime;
  if (definition === null) {
    const discovery = await execute("command -v bun");
    const executable = discovery.stdout.trim();
    if (discovery.exit !== 0 || !isCanonicalAbsoluteExecutable(executable)) {
      throw runtimeUnavailableFailure("worker Bun executable is unavailable");
    }
    runtime = { executable, mode: "source" };
  } else {
    runtime = requireSourceWorkerServiceRuntime(definition, platform);
  }
  const executable = posixShellQuote(runtime.executable);
  const probe = await execute(
    `test -x ${executable} && ${executable} -e ${posixShellQuote(RUNTIME_PROBE_PROGRAM)}`,
  );
  if (probe.exit !== 0) {
    throw runtimeUnavailableFailure("installed worker Bun executable is unavailable");
  }
  return parseRuntimeObservation(
    runtime.executable,
    runtime.mode,
    probe.stdout.trim(),
    platform,
  );
}

/** Pins the local installed service executable, with discovery only for a first install. */
export function resolveLocalWorkerRuntime(
  definition: string | null,
  platform: WorkerServicePlatform,
): WorkerRuntimeObservation {
  const runtime = definition === null
    ? {
        executable: Bun.which("bun") ?? process.execPath,
        mode: "source" as const,
      }
    : requireSourceWorkerServiceRuntime(definition, platform);
  if (!isCanonicalAbsoluteExecutable(runtime.executable)
    || !existsSync(runtime.executable)) {
    throw runtimeUnavailableFailure("installed worker Bun executable is unavailable");
  }
  const probe = Bun.spawnSync(
    [runtime.executable, "-e", RUNTIME_PROBE_PROGRAM],
    { stderr: "pipe", stdout: "pipe" },
  );
  if (probe.exitCode !== 0) {
    throw runtimeUnavailableFailure("installed worker Bun executable is unavailable");
  }
  return parseRuntimeObservation(
    runtime.executable,
    runtime.mode,
    probe.stdout.toString().trim(),
    platform,
  );
}
