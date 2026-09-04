import { describe, expect, test } from "bun:test";
import {
  chmodSync,
  chownSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createServer, type Server } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { verifySystemdUnitSyntax } from "./systemd-unit-verification.ts";

const REPO_ROOT = resolve(import.meta.dir, "../../..");
const SYSTEMD_DIR = resolve(REPO_ROOT, "assets/linux/systemd");
const readAsset = (path: string): string => readFileSync(resolve(REPO_ROOT, path), "utf8");
const directiveValues = (text: string, name: string): string[] => text
  .split("\n")
  .filter((line) => line.startsWith(`${name}=`))
  .map((line) => line.slice(name.length + 1));

const authUnit = readAsset("assets/linux/systemd/roost-saas-auth.service");
const provisionerUnit = readAsset("assets/linux/systemd/roost-saas-provisioner.service");
const provisionerServer = readAsset("apps/roost-cli/src/saas-provisioner/server.ts");
const systemdAnalyze = Bun.which("systemd-analyze");
const setpriv = Bun.which("setpriv");
const node = Bun.which("node");

const CHILD_SOURCE = `
"use strict";
const fs = require("node:fs");
const net = require("node:net");

const fixture = JSON.parse(process.env.ROOST_SANDBOX_FIXTURE);

function readAttempt(path) {
  try {
    fs.readFileSync(path);
    return { allowed: true };
  } catch (error) {
    return { allowed: false, code: error && error.code ? String(error.code) : "UNKNOWN" };
  }
}

function connectAttempt(path) {
  const { promise, resolve } = Promise.withResolvers();
  const socket = net.createConnection(path);
  let body = "";
  let settled = false;
  const finish = (result) => {
    if (settled) return;
    settled = true;
    socket.destroy();
    resolve(result);
  };
  socket.setEncoding("utf8");
  socket.on("data", (chunk) => { body += chunk; });
  socket.once("error", (error) => finish({
    allowed: false,
    code: error && error.code ? String(error.code) : "UNKNOWN",
  }));
  socket.once("end", () => finish({ allowed: true, body }));
  return promise;
}

(async () => {
  const result = {
    role: fixture.role,
    uid: process.getuid(),
    gid: process.getgid(),
    reads: {},
  };
  if (fixture.role === "roost-signup-identity") {
    for (const [name, path] of Object.entries(fixture.privatePaths)) {
      result.reads[name] = readAttempt(path);
    }
    result.dockerSocket = await connectAttempt(fixture.dockerSocket);
    result.provisionSocket = await connectAttempt(fixture.provisionSocket);
    try {
      fs.writeFileSync(fixture.stateProbe, "signup-state-only\\n", { flag: "wx" });
      result.state = { allowed: true, body: fs.readFileSync(fixture.stateProbe, "utf8") };
    } catch (error) {
      result.state = {
        allowed: false,
        code: error && error.code ? String(error.code) : "UNKNOWN",
      };
    }
  } else if (fixture.role === "caddy-identity") {
    result.provisionSocket = await connectAttempt(fixture.provisionSocket);
  } else {
    throw new Error("unknown fixture identity");
  }
  process.stdout.write(JSON.stringify(result));
})().catch((error) => {
  process.stderr.write(error && error.stack ? error.stack : String(error));
  process.exitCode = 1;
});
`;

type AccessResult = { allowed: boolean; code?: string; body?: string };
type IdentityResult = {
  role: string;
  uid: number;
  gid: number;
  reads: Record<string, AccessResult>;
  dockerSocket?: AccessResult;
  provisionSocket: AccessResult;
  state?: AccessResult;
};

function unusedNumericIdentities(count: number): number[] {
  const used = new Set<number>([0]);
  for (const line of readFileSync("/etc/passwd", "utf8").split("\n")) {
    const uid = Number.parseInt(line.split(":")[2] ?? "", 10);
    if (Number.isSafeInteger(uid)) used.add(uid);
  }
  for (const line of readFileSync("/etc/group", "utf8").split("\n")) {
    const gid = Number.parseInt(line.split(":")[2] ?? "", 10);
    if (Number.isSafeInteger(gid)) used.add(gid);
  }
  const result: number[] = [];
  for (let candidate = 60_000; candidate >= 50_000 && result.length < count; candidate -= 1) {
    if (!used.has(candidate)) result.push(candidate);
  }
  if (result.length !== count) throw new Error("could not allocate unused numeric identities for DAC fixture");
  return result;
}

function makeDirectory(path: string, mode: number, uid: number, gid: number): void {
  mkdirSync(path, { mode });
  chownSync(path, uid, gid);
  chmodSync(path, mode);
}

function makeFile(path: string, content: string, mode: number, uid: number, gid: number): void {
  writeFileSync(path, content, { mode });
  chownSync(path, uid, gid);
  chmodSync(path, mode);
}

async function listenFixtureSocket(path: string, gid: number, response: string): Promise<Server> {
  const server = createServer((socket) => socket.end(response));
  const { promise, resolve: resolveListen, reject: rejectListen } = Promise.withResolvers<void>();
  const onError = (error: Error) => rejectListen(error);
  server.once("error", onError);
  server.listen(path, () => {
    server.off("error", onError);
    resolveListen();
  });
  await promise;
  chownSync(path, 0, gid);
  chmodSync(path, 0o660);
  return server;
}

async function closeServer(server: Server | undefined): Promise<void> {
  if (!server) return;
  const { promise, resolve: resolveClose } = Promise.withResolvers<void>();
  server.close(() => resolveClose());
  await promise;
}

async function runIdentity(
  childPath: string,
  cwd: string,
  uid: number,
  gid: number,
  fixture: Record<string, unknown>,
): Promise<IdentityResult> {
  const child = Bun.spawn({
    cmd: [
      setpriv as string,
      `--reuid=${uid}`,
      `--regid=${gid}`,
      "--clear-groups",
      "--no-new-privs",
      node as string,
      childPath,
    ],
    cwd,
    env: {
      PATH: "/usr/bin:/bin",
      ROOST_SANDBOX_FIXTURE: JSON.stringify(fixture),
    },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const stdout = new Response(child.stdout).text();
  const stderr = new Response(child.stderr).text();
  try {
    const exitCode = await child.exited;
    const [output, errorOutput] = await Promise.all([stdout, stderr]);
    if (exitCode !== 0) throw new Error(`sandbox identity child failed (${exitCode}): ${errorOutput}`);
    return JSON.parse(output) as IdentityResult;
  } finally {
    if (child.exitCode === null) {
      child.kill("SIGKILL");
      await child.exited;
    }
  }
}

function expectDenied(result: AccessResult | undefined): void {
  expect(result).toBeDefined();
  if (result === undefined) throw new Error("sandbox access result was absent");
  expect(result.allowed).toBe(false);
  expect(result.code).toBeDefined();
  if (result.code === undefined) throw new Error("sandbox denial omitted its error code");
  expect(["EACCES", "EPERM"]).toContain(result.code);
}

function expectNode(path: string, mode: number, uid: number, gid: number): void {
  const stat = statSync(path);
  expect(stat.mode & 0o777).toBe(mode);
  expect(stat.uid).toBe(uid);
  expect(stat.gid).toBe(gid);
}

const runtimeSkipReason = process.platform !== "linux"
  ? "requires Linux DAC and setpriv"
  : typeof process.geteuid !== "function" || process.geteuid() !== 0
    ? "requires root to enter synthetic service identities"
    : setpriv === null
      ? "requires util-linux setpriv"
      : node === null
        ? "requires a Node child runtime"
        : null;

describe("SaaS auth runtime sandbox", () => {
  test("pins the signup and provisioner systemd confinement boundary", () => {
    expect(authUnit).toContain("User=roost-signup\nGroup=roost-signup");
    expect(authUnit).toContain("StateDirectory=roost-signup\nStateDirectoryMode=0700");
    expect(authUnit).toContain("CapabilityBoundingSet=\nAmbientCapabilities=");
    expect(authUnit).toContain("ProtectSystem=strict");
    expect(authUnit).toContain("InaccessiblePaths=/run/docker.sock /srv/data/roost /srv/infra/edge");

    expect(provisionerUnit).toContain("User=root\nGroup=roost-signup");
    expect(provisionerUnit).toContain("RuntimeDirectory=roost-saas-private\nRuntimeDirectoryMode=0750");
    expect(directiveValues(provisionerUnit, "SocketBindDeny")).toEqual([
      "ipv4:tcp",
      "ipv6:tcp",
    ]);
    expect(provisionerUnit).not.toContain("ListenStream=");
  });

  test("provisioner server has one Unix-path listener and no TCP listener", () => {
    const listenArguments = [...provisionerServer.matchAll(/\bserver\.listen\(([^)\n]+)\);/g)]
      .map((match) => match[1]!.trim());
    expect(provisionerServer).toContain("socketPath: string;");
    expect(listenArguments).toEqual(["this.#options.socketPath"]);
    expect(provisionerServer).not.toMatch(/\.listen\(\s*\{[^}]*\b(?:host|port)\b/s);
  });

  test.skipIf(systemdAnalyze === null)(
    systemdAnalyze === null
      ? "systemd accepts auth sandbox units [skipped: systemd-analyze is unavailable]"
      : "systemd accepts auth sandbox units",
    () => {
      const units = [
        "roost-saas-auth.service",
        "roost-saas-provisioner.service",
        "roost-saas-auth-bridge.service",
        "roost-saas-auth-bridge.socket",
        "roost-saas-origin-isolation.service",
        "roost-saas-resolver.service",
        "roost-saas-resolver-bridge.service",
        "roost-saas-resolver-bridge.socket",
      ].map((name) => resolve(SYSTEMD_DIR, name));
      expect(verifySystemdUnitSyntax(systemdAnalyze as string, units)).toBe(0);
    },
  );

  test.skipIf(runtimeSkipReason !== null)(
    runtimeSkipReason === null
      ? "synthetic UID DAC limits roost-signup to its state and provision socket"
      : `synthetic UID DAC limits roost-signup to its state and provision socket [skipped: ${runtimeSkipReason}]`,
    async () => {
      const root = mkdtempSync(join(tmpdir(), "roost-saas-auth-sandbox-"));
      chmodSync(root, 0o755);
      const [signupId, caddyId, dockerGroup, edgeGroup] = unusedNumericIdentities(4);
      let dockerServer: Server | undefined;
      let provisionServerFixture: Server | undefined;
      try {
        const controlDb = join(root, "control.db");
        makeFile(controlDb, "control-plane\n", 0o600, 0, 0);

        const dockerDir = join(root, "docker");
        makeDirectory(dockerDir, 0o755, 0, 0);
        const dockerSocket = join(dockerDir, "docker.sock");
        dockerServer = await listenFixtureSocket(dockerSocket, dockerGroup!, "docker-fixture");

        const edgeDir = join(root, "edge");
        makeDirectory(edgeDir, 0o750, 0, edgeGroup!);
        const edgeConfig = join(edgeDir, "roost-tenants.caddy");
        makeFile(edgeConfig, "tenant fixture.invalid\n", 0o640, 0, edgeGroup!);

        const tenantDir = join(root, "tenant");
        makeDirectory(tenantDir, 0o700, 0, 0);
        const tenantSecret = join(tenantDir, "coordinator-key");
        makeFile(tenantSecret, "tenant-private\n", 0o600, 0, 0);

        const credentialDir = join(root, "saas-auth");
        makeDirectory(credentialDir, 0o700, 0, 0);
        const assertionPrivate = join(credentialDir, "assertion-signing-key");
        const assertionPublic = join(credentialDir, "assertion-verify-key");
        makeFile(assertionPrivate, "private-fixture\n", 0o600, 0, 0);
        makeFile(assertionPublic, "public-fixture\n", 0o644, 0, 0);

        const provisionDir = join(root, "roost-saas-private");
        makeDirectory(provisionDir, 0o750, 0, signupId!);
        const provisionSocket = join(provisionDir, "provision.sock");
        provisionServerFixture = await listenFixtureSocket(provisionSocket, signupId!, "provision-ok");

        const stateDir = join(root, "roost-signup-state");
        makeDirectory(stateDir, 0o700, signupId!, signupId!);
        const stateProbe = join(stateDir, "child-state.txt");
        const childPath = join(root, "identity-child.cjs");
        makeFile(childPath, CHILD_SOURCE, 0o644, 0, 0);

        expectNode(controlDb, 0o600, 0, 0);
        expectNode(dockerSocket, 0o660, 0, dockerGroup!);
        expect(statSync(dockerSocket).isSocket()).toBe(true);
        expectNode(edgeConfig, 0o640, 0, edgeGroup!);
        expectNode(tenantDir, 0o700, 0, 0);
        expectNode(assertionPrivate, 0o600, 0, 0);
        expectNode(assertionPublic, 0o644, 0, 0);
        expectNode(provisionDir, 0o750, 0, signupId!);
        expectNode(provisionSocket, 0o660, 0, signupId!);
        expect(statSync(provisionSocket).isSocket()).toBe(true);
        expectNode(stateDir, 0o700, signupId!, signupId!);

        const privatePaths = {
          controlDb,
          edgeConfig,
          tenantSecret,
          assertionPrivate,
          assertionPublic,
        };
        const signup = await runIdentity(childPath, stateDir, signupId!, signupId!, {
          role: "roost-signup-identity",
          privatePaths,
          dockerSocket,
          provisionSocket,
          stateProbe,
        });
        expect(signup).toMatchObject({
          role: "roost-signup-identity",
          uid: signupId,
          gid: signupId,
        });
        for (const result of Object.values(signup.reads)) expectDenied(result);
        expect(Object.keys(signup.reads).sort()).toEqual(Object.keys(privatePaths).sort());
        expectDenied(signup.dockerSocket);
        expect(signup.provisionSocket).toEqual({ allowed: true, body: "provision-ok" });
        expect(signup.state).toEqual({ allowed: true, body: "signup-state-only\n" });
        expect(readFileSync(stateProbe, "utf8")).toBe("signup-state-only\n");

        const caddy = await runIdentity(childPath, root, caddyId!, caddyId!, {
          role: "caddy-identity",
          provisionSocket,
        });
        expect(caddy).toMatchObject({ role: "caddy-identity", uid: caddyId, gid: caddyId });
        expectDenied(caddy.provisionSocket);
      } finally {
        await closeServer(provisionServerFixture);
        await closeServer(dockerServer);
        rmSync(root, { recursive: true, force: true });
      }
    },
  );
});
