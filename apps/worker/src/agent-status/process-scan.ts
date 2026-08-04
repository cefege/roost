// Adapted from Herdr process-backed agent detection at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).

import { log } from "@roost/shared";

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

function executableName(value: string): string {
  const clean = value.replace(/^["']|["'],?$/g, "");
  const slash = clean.lastIndexOf("/");
  return (slash >= 0 ? clean.slice(slash + 1) : clean).replace(/\.(?:js|mjs|cjs|ts)$/, "");
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

export function identifyAgentProcess(record: ProcessRecord): BuiltinAgentId | null {
  const argv = record.args.trim().split(/\s+/).filter(Boolean);
  let commandIndex = 0;
  if (executableName(argv[0] ?? "") === "env") {
    commandIndex++;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[commandIndex] ?? "")) commandIndex++;
  }
  const command = argv[commandIndex] ?? "";
  const commandName = executableName(command);
  const candidates = [executableName(record.comm), commandName];
  let scriptPath = "";
  if (RUNTIME_COMMANDS.includes(commandName)) {
    let scriptIndex = commandIndex + 1;
    while ((argv[scriptIndex] ?? "").startsWith("-")) scriptIndex++;
    scriptPath = (argv[scriptIndex] ?? "").replace(/^["']|["']$/g, "");
    candidates.push(executableName(scriptPath));
  }
  for (const [agentId, commands] of Object.entries(BUILTIN_AGENT_COMMANDS) as Array<
    [BuiltinAgentId, readonly string[]]
  >) {
    if (candidates.some((candidate) => commands.some((name) => name === candidate))) return agentId;
    if (scriptPath && AGENT_PACKAGE_MARKERS[agentId].some((marker) => scriptPath.includes(marker))) return agentId;
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
    const agentId = identifyAgentProcess(record);
    if (!agentId) continue;
    const exactCommand = BUILTIN_AGENT_COMMANDS[agentId].some(
      (name) => name === executableName(record.comm),
    );
    const foreground = record.tpgid > 0 && record.pgid === record.tpgid;
    const score = (exactCommand ? 100 : 0) + (foreground ? 50 : 0) + depth;
    if (!best || score > best.score) best = { identity: { agentId, pid: record.pid }, score };
  }
  return best?.identity ?? null;
}

interface HeldIdentity extends AgentProcessIdentity { misses: number }

export type ProcessSnapshotReader = () => Promise<ProcessRecord[]>;

async function readProcessSnapshot(): Promise<ProcessRecord[]> {
  const proc = Bun.spawn([
    "ps", "-A", "-o", "pid=,ppid=,pgid=,tpgid=,comm=,args=",
  ], { stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  if (exitCode !== 0) throw new Error(stderr.trim() || `ps exited ${exitCode}`);
  return parsePsSnapshot(stdout);
}

export class AgentProcessScanner {
  private records: ProcessRecord[] = [];
  private scannedAt = 0;
  private scanPromise: Promise<boolean> | null = null;
  private heldBySession = new Map<string, HeldIdentity>();
  constructor(
    private readonly readSnapshot: ProcessSnapshotReader = readProcessSnapshot,
    private readonly throttleMs = SCAN_THROTTLE_MS,
  ) {}

  private async refresh(now = Date.now(), force = false): Promise<boolean> {
    if (!force && now - this.scannedAt < this.throttleMs) return true;
    if (this.scanPromise) return this.scanPromise;
    this.scanPromise = (async () => {
      try {
        this.records = await this.readSnapshot();
        this.scannedAt = Date.now();
        return true;
      } catch (error) {
        log.warn("agent-status", "process_scan_failed", { error: String(error) });
        return false;
      } finally {
        this.scanPromise = null;
      }
    })();
    return this.scanPromise;
  }

  async scanAgents(roots: readonly SessionProcessRoot[]): Promise<Map<string, AgentProcessIdentity>> {
    const refreshed = await this.refresh();
    const liveIds = new Set(roots.map((root) => root.sessionId));
    for (const sessionId of this.heldBySession.keys()) {
      if (!liveIds.has(sessionId)) this.heldBySession.delete(sessionId);
    }
    const result = new Map<string, AgentProcessIdentity>();
    for (const root of roots) {
      const detected = findAgentProcessIdentity(this.records, root.childPid);
      const held = this.heldBySession.get(root.sessionId);
      if (detected) {
        this.heldBySession.set(root.sessionId, { ...detected, misses: 0 });
        result.set(root.sessionId, detected);
      } else if (!refreshed && held) {
        result.set(root.sessionId, held);
      } else if (held && held.misses < 1) {
        held.misses++;
        result.set(root.sessionId, held);
      } else {
        this.heldBySession.delete(root.sessionId);
      }
    }
    return result;
  }

  async sessionForPid(pid: number, roots: readonly SessionProcessRoot[]): Promise<string | null> {
    const resolve = (): string | null => {
      const rootByPid = new Map(roots.map((root) => [root.childPid, root.sessionId]));
      const byPid = new Map(this.records.map((record) => [record.pid, record]));
      const visited = new Set<number>();
      let current = pid;
      while (current > 0 && !visited.has(current)) {
        const sessionId = rootByPid.get(current);
        if (sessionId) return sessionId;
        visited.add(current);
        current = byPid.get(current)?.ppid ?? 0;
      }
      return null;
    };

    await this.refresh();
    const resolved = resolve();
    if (resolved) return resolved;
    // A reporter can start in the 250 ms after the detector's last global
    // snapshot. One on-demand retry closes that race; reports are rare, while
    // the hot screen detector remains throttled to one `ps` per interval.
    await this.refresh(Date.now(), true);
    return resolve();
  }
}
