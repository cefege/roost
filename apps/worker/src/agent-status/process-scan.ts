// Adapted from Herdr process-backed agent detection at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).
// Forced proof refreshes abort the snapshot and terminate a spawned ps process.

import { log } from "@roost/shared/log";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { windowsProcessSnapshot } from "@roost/shared/windows-helper";

const HOST_PLATFORM = supportedHostPlatform();

export const BUILTIN_AGENT_COMMANDS = {
  codex: ["codex"],
  gemini: ["gemini"],
  opencode: ["opencode", "open-code"],
  cursor: ["cursor-agent"],
  amp: ["amp", "amp-local"],
  copilot: ["copilot", "github-copilot", "ghcs"],
  droid: ["droid"],
  grok: ["grok", "grok-build"],
  pi: ["pi"],
  omp: ["omp"],
} as const;

export type BuiltinAgentId = keyof typeof BUILTIN_AGENT_COMMANDS;

export interface ProcessRecord {
  pid: number;
  ppid: number;
  pgid: number;
  tpgid: number;
  comm: string;
  args: string;
}

export interface SessionProcessRoot {
  sessionId: string;
  childPid: number;
}

export interface AgentProcessIdentity {
  agentId: BuiltinAgentId;
  pid: number;
}

const SCAN_THROTTLE_MS = 250;

export function parsePsSnapshot(output: string): ProcessRecord[] {
  const records: ProcessRecord[] = [];
  for (const line of output.split("\n")) {
    const match = /^\s*(\d+)\s+(\d+)\s+(-?\d+)\s+(-?\d+)\s+(\S+)\s*(.*)$/.exec(line);
    if (!match) continue;
    records.push({
      pid: Number(match[1]),
      ppid: Number(match[2]),
      pgid: Number(match[3]),
      tpgid: Number(match[4]),
      comm: match[5]!,
      args: match[6] ?? "",
    });
  }
  return records;
}

function executableName(value: string, platform: SupportedHostPlatform): string {
  const clean = value.trim().replace(/^["']|["'],?$/g, "");
  const slash = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  let name = (slash >= 0 ? clean.slice(slash + 1) : clean)
    .replace(/\.(?:js|mjs|cjs|ts)$/, "");
  if (platform === "win32") {
    name = name.replace(/\.(?:exe|cmd|bat|com)$/i, "").toLocaleLowerCase("en-US");
  }
  return name;
}

function windowsCommandLineArgs(commandLine: string): string[] {
  const args: string[] = [];
  let current = "";
  let started = false;
  let quoted = false;
  for (let index = 0; index < commandLine.length;) {
    const char = commandLine[index]!;
    if ((char === " " || char === "\t") && !quoted) {
      if (started) {
        args.push(current);
        current = "";
        started = false;
      }
      index++;
      continue;
    }
    if (char === "\\") {
      let end = index;
      while (commandLine[end] === "\\") end++;
      const count = end - index;
      if (commandLine[end] === "\"") {
        current += "\\".repeat(Math.floor(count / 2));
        started = true;
        if (count % 2 === 1) {
          current += "\"";
        } else if (quoted && commandLine[end + 1] === "\"") {
          current += "\"";
          end++;
        } else {
          quoted = !quoted;
        }
        index = end + 1;
        continue;
      }
      current += "\\".repeat(count);
      started = true;
      index = end;
      continue;
    }
    if (char === "\"") {
      started = true;
      if (quoted && commandLine[index + 1] === "\"") {
        current += "\"";
        index += 2;
      } else {
        quoted = !quoted;
        index++;
      }
      continue;
    }
    current += char;
    started = true;
    index++;
  }
  if (started) args.push(current);
  return args;
}

const RUNTIME_COMMANDS = ["node", "nodejs", "bun", "deno"];
const AGENT_PACKAGE_MARKERS: Readonly<Record<BuiltinAgentId, readonly string[]>> = {
  codex: ["/@openai/codex/", "/codex/"],
  gemini: ["/@google/gemini-cli/", "/gemini-cli/"],
  opencode: ["/opencode/"],
  cursor: ["/cursor-agent/"],
  amp: ["/@sourcegraph/amp/", "/amp/"],
  copilot: ["/@github/copilot/", "/github-copilot/"],
  droid: ["/droid/"],
  grok: ["/grok-build/", "/grok/"],
  pi: ["/@mariozechner/pi-coding-agent/", "/@badlogic/pi-coding-agent/"],
  omp: ["/@oh-my-pi/pi-coding-agent/", "/oh-my-pi/"],
};

export function identifyAgentProcess(
  record: ProcessRecord,
  platform: SupportedHostPlatform = HOST_PLATFORM,
): BuiltinAgentId | null {
  const argv = platform === "win32"
    ? windowsCommandLineArgs(record.args)
    : record.args.trim().split(/\s+/).filter(Boolean);
  let commandIndex = 0;
  if (executableName(argv[0] ?? "", platform) === "env") {
    commandIndex++;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[commandIndex] ?? "")) commandIndex++;
  }
  const command = argv[commandIndex] ?? "";
  const commandName = executableName(command, platform);
  const candidates = [executableName(record.comm, platform), commandName];
  let scriptPath = "";
  if (RUNTIME_COMMANDS.includes(commandName)) {
    let scriptIndex = commandIndex + 1;
    while ((argv[scriptIndex] ?? "").startsWith("-")) scriptIndex++;
    scriptPath = (argv[scriptIndex] ?? "").replace(/^["']|["']$/g, "");
    candidates.push(executableName(scriptPath, platform));
  }
  const normalizedScriptPath = platform === "win32"
    ? scriptPath.replace(/\\/g, "/").toLocaleLowerCase("en-US")
    : scriptPath;
  for (const [agentId, commands] of Object.entries(BUILTIN_AGENT_COMMANDS) as Array<
    [BuiltinAgentId, readonly string[]]
  >) {
    if (candidates.some((candidate) => commands.some((name) => name === candidate))) return agentId;
    if (
      normalizedScriptPath &&
      AGENT_PACKAGE_MARKERS[agentId].some((marker) => normalizedScriptPath.includes(marker))
    ) return agentId;
  }
  return null;
}

function descendants(records: readonly ProcessRecord[], rootPid: number): ProcessRecord[] {
  const children = new Map<number, ProcessRecord[]>();
  for (const record of records) {
    let list = children.get(record.ppid);
    if (!list) children.set(record.ppid, list = []);
    list.push(record);
  }
  const root = records.find((record) => record.pid === rootPid);
  const out: ProcessRecord[] = root ? [root] : [];
  for (let index = 0; index < out.length; index++) {
    const list = children.get(out[index]!.pid);
    if (list) out.push(...list);
  }
  return out;
}

export function findAgentProcessIdentity(
  records: readonly ProcessRecord[],
  rootPid: number,
): AgentProcessIdentity | null {
  let best: { identity: AgentProcessIdentity; score: number } | null = null;
  const tree = descendants(records, rootPid);
  for (let depth = 0; depth < tree.length; depth++) {
    const record = tree[depth]!;
    const agentId = identifyAgentProcess(record, HOST_PLATFORM);
    if (!agentId) continue;
    const exactCommand = BUILTIN_AGENT_COMMANDS[agentId].some(
      (name) => name === executableName(record.comm, HOST_PLATFORM),
    );
    const foreground = HOST_PLATFORM !== "win32" && record.tpgid > 0 && record.pgid === record.tpgid;
    const score = (exactCommand ? 100 : 0) + (foreground ? 50 : 0) + depth;
    if (!best || score > best.score) best = { identity: { agentId, pid: record.pid }, score };
  }
  return best?.identity ?? null;
}

function findExactAgentProcessIdentity(
  records: readonly ProcessRecord[],
  rootPid: number,
  processId: number,
): AgentProcessIdentity | null {
  const record = descendants(records, rootPid).find((candidate) => candidate.pid === processId);
  if (!record) return null;
  const agentId = identifyAgentProcess(record, HOST_PLATFORM);
  return agentId ? { agentId, pid: processId } : null;
}

interface HeldIdentity extends AgentProcessIdentity { misses: number }
export type ProcessSnapshotReader = (signal?: AbortSignal) => Promise<ProcessRecord[]>;

export async function _readProcessSnapshot(signal?: AbortSignal): Promise<ProcessRecord[]> {
  if (signal?.aborted) throw new Error("process snapshot aborted");
  switch (HOST_PLATFORM) {
    case "darwin":
    case "linux": {
      const proc = Bun.spawn([
        "ps", "-A", "-o", "pid=,ppid=,pgid=,tpgid=,comm=,args=",
      ], { stdout: "pipe", stderr: "pipe" });
      const terminate = () => {
        try { proc.kill("SIGKILL"); } catch { /* already exited */ }
      };
      signal?.addEventListener("abort", terminate, { once: true });
      try {
        const [stdout, stderr, exitCode] = await Promise.all([
          new Response(proc.stdout).text(),
          new Response(proc.stderr).text(),
          proc.exited,
        ]);
        if (signal?.aborted) throw new Error("process snapshot aborted");
        if (exitCode !== 0) throw new Error(stderr.trim() || `ps exited ${exitCode}`);
        return parsePsSnapshot(stdout);
      } finally {
        signal?.removeEventListener("abort", terminate);
      }
    }
    case "win32":
      return windowsProcessSnapshot();
    default:
      return assertNeverPlatform(HOST_PLATFORM);
  }
}

export class AgentProcessScanner {
  private records: ProcessRecord[] = [];
  private scannedAt = 0;
  private scanPromise: Promise<boolean> | null = null;
  private scanController: AbortController | null = null;
  private heldBySession = new Map<string, HeldIdentity>();
  constructor(
    private readonly readSnapshot: ProcessSnapshotReader = _readProcessSnapshot,
    private readonly throttleMs = SCAN_THROTTLE_MS,
  ) {}

  private async awaitScan(
    scan: Promise<boolean>,
    signal?: AbortSignal,
  ): Promise<boolean> {
    if (!signal) return scan;
    const abortCurrentScan = () => {
      if (this.scanPromise !== scan) return;
      const controller = this.scanController;
      this.scanPromise = null;
      this.scanController = null;
      controller?.abort();
    };
    if (signal.aborted) {
      abortCurrentScan();
      return false;
    }
    const { promise: aborted, resolve: resolveAborted } = Promise.withResolvers<boolean>();
    const onAbort = () => {
      abortCurrentScan();
      resolveAborted(false);
    };
    signal.addEventListener("abort", onAbort, { once: true });
    try {
      return await Promise.race([scan, aborted]);
    } finally {
      signal.removeEventListener("abort", onAbort);
    }
  }

  private async refresh(
    now = Date.now(),
    force = false,
    signal?: AbortSignal,
  ): Promise<boolean> {
    if (signal?.aborted) return false;
    if (!force && now - this.scannedAt < this.throttleMs) return true;
    if (this.scanPromise) return this.awaitScan(this.scanPromise, signal);
    const controller = new AbortController();
    const scan: Promise<boolean> = (async () => {
      try {
        const records = await Promise.resolve().then(
          () => this.readSnapshot(controller.signal),
        );
        if (controller.signal.aborted) return false;
        this.records = records;
        this.scannedAt = Date.now();
        return true;
      } catch (error) {
        if (!controller.signal.aborted) {
          log.warn("agent-status", "process_scan_failed", { error: String(error) });
        }
        return false;
      } finally {
        if (this.scanController === controller) {
          this.scanPromise = null;
          this.scanController = null;
        }
      }
    })();
    this.scanPromise = scan;
    this.scanController = controller;
    return this.awaitScan(scan, signal);
  }

  async scanAgents(roots: readonly SessionProcessRoot[]): Promise<Map<string, AgentProcessIdentity>> {
    const refreshed = await this.refresh();
    const liveIds = new Set(roots.map((root) => root.sessionId));
    for (const sessionId of this.heldBySession.keys()) {
      if (!liveIds.has(sessionId)) this.heldBySession.delete(sessionId);
    }
    const result = new Map<string, AgentProcessIdentity>();
    for (const root of roots) {
      const held = this.heldBySession.get(root.sessionId);
      const liveHeld = held
        ? findExactAgentProcessIdentity(this.records, root.childPid, held.pid)
        : null;
      if (held && liveHeld?.agentId === held.agentId) {
        held.misses = 0;
        result.set(root.sessionId, liveHeld);
        continue;
      }
      if (!refreshed && held) {
        result.set(root.sessionId, held);
        continue;
      }
      if (held && held.misses < 1) {
        held.misses++;
        result.set(root.sessionId, held);
        continue;
      }
      const detected = findAgentProcessIdentity(this.records, root.childPid);
      if (detected) {
        this.heldBySession.set(root.sessionId, { ...detected, misses: 0 });
        result.set(root.sessionId, detected);
      } else {
        this.heldBySession.delete(root.sessionId);
      }
    }
    return result;
  }

  async scanReportingAgent(
    root: SessionProcessRoot,
    reporterPid: number,
    signal?: AbortSignal,
  ): Promise<AgentProcessIdentity | null> {
    const activeScan = this.scanPromise;
    if (activeScan && !(await this.awaitScan(activeScan, signal))) return null;
    const refreshed = await this.refresh(Date.now(), true, signal);
    if (!refreshed) return null;
    const held = this.heldBySession.get(root.sessionId);
    if (held) {
      const liveHeld = findExactAgentProcessIdentity(this.records, root.childPid, held.pid);
      if (!liveHeld || liveHeld.agentId !== held.agentId) return null;
      return liveHeld.pid === reporterPid ? liveHeld : null;
    }
    return null;
  }
}
