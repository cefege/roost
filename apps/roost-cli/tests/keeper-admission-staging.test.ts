// Pins what a deploy driver may stage over an already-installed worker service.
// A refusal from the registry stands only while the target itself proves it
// still runs a worker or a keeper holding channels. The generated probe is run
// by a real shell against stub launchctl/systemctl/pgrep binaries, so the
// command text is exercised rather than paraphrased.

import { afterEach, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { DeployWorkerOs } from "../src/deploy-exec.ts";
import { MACOS_WORKER_PLIST_RELATIVE } from "../src/deploy-macos-journal-commands.ts";
import {
  installedServiceRefusalAfterTargetEvidence,
  keeperAdmissionStaging,
} from "../src/keeper-admission-staging.ts";
import {
  MACOS_KEEPER_UPDATE,
  MACOS_WORKER_FINGERPRINT,
} from "./deploy-macos-keeper-update-fixture.ts";

const HOST = "mike-m5-air";
const LINUX_UNIT = "/home/worker/.config/systemd/user/roost-worker.service";
const RUNNING_LAUNCHD_JOB = "\tstate = running\n\tpid = 4711\n\tactive count = 1";
const RUNNING_SYSTEMD_SHOW = "ActiveState=active\nSubState=running\nMainPID=4711";
const STOPPED_SYSTEMD_SHOW = "ActiveState=inactive\nSubState=dead\nMainPID=0";

const temporaryRoots: string[] = [];

interface KeeperProcess {
  pid: number;
  children: readonly number[];
}

interface TargetStub {
  os: DeployWorkerOs;
  serviceInstalled: boolean;
  /** Raw service-manager output; its exit code is `serviceQueryExit`. */
  serviceOutput: string;
  serviceQueryExit: number;
  /** Darwin only: `launchctl print-disabled` proves launchd is reachable. */
  launchdDomainExit?: number;
  /** `null` models a target whose PATH has no pgrep at all. */
  keepers: readonly KeeperProcess[] | null;
}

const PGREP_STUB = [
  "#!/bin/sh",
  'case "$1" in',
  '  -f) printf %s "$ROOST_STUB_KEEPER_PIDS"; test -n "$ROOST_STUB_KEEPER_PIDS" ;;',
  '  -P) printf %s "$2" > "$ROOST_STUB_PARENTS"; printf %s "$ROOST_STUB_KEEPER_CHILDREN";',
  '      test -n "$ROOST_STUB_KEEPER_CHILDREN" ;;',
  "  *) exit 1 ;;",
  "esac",
].join("\n");

const LAUNCHCTL_STUB = [
  "#!/bin/sh",
  'if [ "$1" = "print-disabled" ]; then exit "$ROOST_STUB_LAUNCHD_DOMAIN_EXIT"; fi',
  'if [ "$1" = "print" ]; then',
  '  printf %s\\\\n "$ROOST_STUB_SERVICE_OUTPUT"',
  '  exit "$ROOST_STUB_SERVICE_EXIT"',
  "fi",
  "exit 1",
].join("\n");

const SYSTEMCTL_STUB = [
  "#!/bin/sh",
  'printf %s\\\\n "$ROOST_STUB_SERVICE_OUTPUT"',
  'exit "$ROOST_STUB_SERVICE_EXIT"',
].join("\n");

// The probe must see only what the scenario declares: this box may itself be
// running a roost keeper, and a real pgrep on PATH would report it.
const TARGET_TOOLS = ["id", "tr", "sed", "grep", "head"] as const;
const BASH = Bun.which("bash") ?? "/bin/bash";

function writeExecutable(path: string, body: string): void {
  writeFileSync(path, `${body}\n`);
  chmodSync(path, 0o755);
}

function stubTarget(stub: TargetStub): {
  home: string;
  serviceSpec: string;
  parentsLog: string;
  environment: Record<string, string>;
} {
  const root = mkdtempSync(join(tmpdir(), "roost-target-evidence-"));
  temporaryRoots.push(root);
  const bin = join(root, "bin");
  const home = join(root, "home");
  mkdirSync(bin, { recursive: true });
  mkdirSync(home, { recursive: true });
  for (const tool of TARGET_TOOLS) {
    const resolved = Bun.which(tool);
    if (!resolved) throw new Error(`target evidence probe needs ${tool}`);
    symlinkSync(resolved, join(bin, tool));
  }
  if (stub.keepers !== null) writeExecutable(join(bin, "pgrep"), PGREP_STUB);
  writeExecutable(
    join(bin, stub.os === "darwin" ? "launchctl" : "systemctl"),
    stub.os === "darwin" ? LAUNCHCTL_STUB : SYSTEMCTL_STUB,
  );

  const serviceSpec = stub.os === "darwin" ? MACOS_WORKER_PLIST_RELATIVE : LINUX_UNIT;
  const servicePath = stub.os === "darwin" ? join(home, serviceSpec) : join(root, "unit");
  if (stub.serviceInstalled) {
    mkdirSync(join(servicePath, ".."), { recursive: true });
    writeFileSync(servicePath, "installed by roost\n");
  }
  const parentsLog = join(root, "pgrep-parents");
  return {
    home,
    serviceSpec: stub.os === "darwin" ? serviceSpec : servicePath,
    parentsLog,
    environment: {
      PATH: bin,
      HOME: home,
      ROOST_STUB_SERVICE_OUTPUT: stub.serviceOutput,
      ROOST_STUB_SERVICE_EXIT: String(stub.serviceQueryExit),
      ROOST_STUB_LAUNCHD_DOMAIN_EXIT: String(stub.launchdDomainExit ?? 0),
      ROOST_STUB_PARENTS: parentsLog,
      ROOST_STUB_KEEPER_PIDS: (stub.keepers ?? []).map(keeper => keeper.pid).join("\n"),
      ROOST_STUB_KEEPER_CHILDREN: (stub.keepers ?? [])
        .flatMap(keeper => keeper.children)
        .join("\n"),
    },
  };
}

/** Resolve the refusal exactly as a deploy driver does: the staging outcome
 * supplies the text, a real shell on the stub target supplies the evidence. */
async function refusalAgainstTarget(
  refusal: string,
  stub: TargetStub,
): Promise<{ refusal: string | null; operatorLines: string[]; childQueryParents: string }> {
  const target = stubTarget(stub);
  const operatorLines: string[] = [];
  const realLog = console.log;
  console.log = (...parts: unknown[]) => { operatorLines.push(parts.join(" ")); };
  try {
    const resolved = await installedServiceRefusalAfterTargetEvidence(refusal, {
      host: HOST,
      os: stub.os,
      serviceSpec: target.serviceSpec,
      execute: async (command) => {
        const child = Bun.spawn([BASH, "-c", command], {
          env: target.environment,
          stdout: "pipe",
          stderr: "pipe",
        });
        const [stdout, stderr] = await Promise.all([
          new Response(child.stdout).text(),
          new Response(child.stderr).text(),
        ]);
        return { exit: await child.exited, stdout, stderr };
      },
    });
    return {
      refusal: resolved,
      operatorLines,
      childQueryParents: existsSync(target.parentsLog)
        ? readFileSync(target.parentsLog, "utf8")
        : "",
    };
  } finally {
    console.log = realLog;
  }
}

function staleProofRefusal(): string {
  const staging = keeperAdmissionStaging(HOST, "macOS", {
    outcome: "proof-stale",
    workerLabel: HOST,
  });
  expect(staging.installedServiceRefusal).toContain("has a stale keeper update proof");
  return staging.installedServiceRefusal!;
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

test("a stale row installs over a service the target proves is running nothing", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "linux",
    serviceInstalled: true,
    serviceOutput: STOPPED_SYSTEMD_SHOW,
    serviceQueryExit: 0,
    keepers: [],
  });

  expect(outcome.refusal).toBeNull();
  expect(outcome.operatorLines).toEqual([
    `>> keeper admission on ${HOST}: staging permitted because the worker service on ${HOST}`
    + " is installed but not running, and its 0 keeper process(es) hold no channel.",
  ]);
});

test("a stale row is still refused while the target runs a worker with keeper channels", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "linux",
    serviceInstalled: true,
    serviceOutput: RUNNING_SYSTEMD_SHOW,
    serviceQueryExit: 0,
    keepers: [{ pid: 5120, children: [5121, 5122] }],
  });

  expect(outcome.refusal).toContain("has a stale keeper update proof");
  expect(outcome.refusal).toContain(`the worker service on ${HOST} is running`);
  expect(outcome.operatorLines).toEqual([]);
});

test("channel processes are counted for every live keeper, not a fixed pid", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "linux",
    serviceInstalled: true,
    serviceOutput: STOPPED_SYSTEMD_SHOW,
    serviceQueryExit: 0,
    keepers: [{ pid: 5120, children: [5121, 5122] }, { pid: 6440, children: [6441] }],
  });

  expect(outcome.childQueryParents).toBe("5120,6440");
  expect(outcome.refusal).toContain("still holds 3 channel process(es)");
});

test("a prior macOS service that is not loaded stages when nothing else runs", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "darwin",
    serviceInstalled: true,
    serviceOutput: "Could not find service in domain",
    serviceQueryExit: 113,
    launchdDomainExit: 0,
    keepers: [{ pid: 6220, children: [] }],
  });

  expect(outcome.refusal).toBeNull();
  expect(outcome.operatorLines[0]).toContain("1 keeper process(es) hold no channel");
});

test("a loaded and running macOS worker is refused even with a stale row", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "darwin",
    serviceInstalled: true,
    serviceOutput: RUNNING_LAUNCHD_JOB,
    serviceQueryExit: 0,
    keepers: [{ pid: 6220, children: [6221] }],
  });

  expect(outcome.refusal).toContain(`the worker service on ${HOST} is running`);
  expect(outcome.operatorLines).toEqual([]);
});

test("an unreachable launchd refuses a plist that is merely not loaded", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "darwin",
    serviceInstalled: true,
    serviceOutput: "Could not find service in domain",
    serviceQueryExit: 113,
    launchdDomainExit: 1,
    keepers: [],
  });

  expect(outcome.refusal).toContain("did not report the worker's state");
});

test("an unreachable service manager refuses instead of reading as not running", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "linux",
    serviceInstalled: true,
    serviceOutput: "Failed to connect to bus: No medium found",
    serviceQueryExit: 1,
    keepers: [],
  });

  expect(outcome.refusal).toContain("did not report the worker's state");
});

test("a target that cannot enumerate processes refuses instead of assuming none", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "linux",
    serviceInstalled: true,
    serviceOutput: STOPPED_SYSTEMD_SHOW,
    serviceQueryExit: 0,
    keepers: null,
  });

  expect(outcome.refusal).toContain("could not prove that no keeper is holding channels");
});

test("a host with no installed worker service stages as a first install", async () => {
  const outcome = await refusalAgainstTarget(staleProofRefusal(), {
    os: "darwin",
    serviceInstalled: false,
    serviceOutput: "Could not find service in domain",
    serviceQueryExit: 113,
    keepers: [],
  });

  expect(outcome.refusal).toBeNull();
  expect(outcome.operatorLines[0]).toContain(`no worker service is installed on ${HOST}`);
});

test("a proven admission carries no refusal for a driver to enforce", () => {
  expect(keeperAdmissionStaging(HOST, "macOS", {
    outcome: "admitted",
    workerFingerprint: MACOS_WORKER_FINGERPRINT,
    keeperUpdate: MACOS_KEEPER_UPDATE,
  })).toEqual({
    keeperUpdate: MACOS_KEEPER_UPDATE,
    workerFingerprint: MACOS_WORKER_FINGERPRINT,
    installedServiceRefusal: null,
  });
});
