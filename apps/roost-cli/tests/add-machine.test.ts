// add-machine coordinator target refusal regression coverage.
// Installed definitions remain authoritative over ambient URL values.
// Invalid targets must fail before the bootstrap RPC can mint a server token.
import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { _addMachine } from "../src/add-machine.ts";

let mintAttempts = 0;
const mintWorkerBootstrap = async (
  _label: string,
  _coordinatorUrl?: string,
): Promise<string> => {
  mintAttempts += 1;
  return "must-not-mint";
};

const ENVIRONMENT_KEYS = [
  "ROOST_COORDINATOR_URL",
  "ROOST_COORDINATOR_PUBLIC_URL",
  "ROOST_WEB_PUBLIC_URL",
  "ROOST_COORD_UNIT",
  "ROOST_COORD_PLIST",
  "ROOST_SERVICE_DIR",
] as const;

let root = "";
let savedEnvironment: Record<string, string | undefined> = {};

function writeInstalledCoordinatorDefinition(coordinatorUrl: string): void {
  if (process.platform === "win32") {
    process.env.ROOST_SERVICE_DIR = root;
    writeFileSync(join(root, "service-definitions.json"), JSON.stringify({
      services: { coordinator: { environment: { ROOST_COORDINATOR_URL: coordinatorUrl } } },
    }));
    return;
  }

  if (process.platform === "darwin") {
    const definitionPath = join(root, "coordinator.plist");
    process.env.ROOST_COORD_PLIST = definitionPath;
    writeFileSync(definitionPath, `<?xml version="1.0"?>
<plist><dict><key>EnvironmentVariables</key><dict>
<key>ROOST_COORDINATOR_URL</key><string>${coordinatorUrl}</string>
</dict></dict></plist>`);
    return;
  }

  const definitionPath = join(root, "roost-coord.service");
  process.env.ROOST_COORD_UNIT = definitionPath;
  writeFileSync(definitionPath, `[Service]\nEnvironment="ROOST_COORDINATOR_URL=${coordinatorUrl}"\n`);
}

beforeEach(() => {
  savedEnvironment = Object.fromEntries(
    ENVIRONMENT_KEYS.map((name) => [name, process.env[name]]),
  );
  for (const name of ENVIRONMENT_KEYS) delete process.env[name];
  process.env.ROOST_COORDINATOR_URL = "https://ambient.example.test";
  root = mkdtempSync(join(tmpdir(), "roost-add-machine-"));
  mintAttempts = 0;
});

afterEach(() => {
  for (const name of ENVIRONMENT_KEYS) {
    const value = savedEnvironment[name];
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  rmSync(root, { recursive: true, force: true });
});

for (const [description, installedUrl] of [
  ["loopback", "http://127.0.0.1:4103"],
  ["malformed remote", "https://coord.example.test/not-an-origin"],
] as const) {
  test(`refuses an installed ${description} URL without minting a worker token`, async () => {
    writeInstalledCoordinatorDefinition(installedUrl);
    const errors: string[] = [];
    const reportError = spyOn(console, "error").mockImplementation((message: unknown) => {
      errors.push(String(message));
    });
    const exit = spyOn(process, "exit").mockImplementation((() => {
      throw new Error("enrollment refused");
    }) as never);

    try {
      await expect(_addMachine(["--platform", "linux"], { mintWorkerBootstrap }))
        .rejects.toThrow("enrollment refused");
      expect(mintAttempts).toBe(0);
      expect(errors).toContain(
        "  Set one on this host's coordinator service, or export it for this command.",
      );
    } finally {
      exit.mockRestore();
      reportError.mockRestore();
    }
  });
}
