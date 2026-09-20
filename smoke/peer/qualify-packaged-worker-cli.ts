// Packaged CLI proof for the packaged-worker release driver.
// It gives the exact artifact a fresh isolated CLI home and checks discovery plus one raw input.
// The browser helper observes the command's terminal output on the production SPA.

import { copyFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { strict as assert } from "node:assert";
import type { TerminalTestStack } from "../terminal/stack.ts";

const CLI_TIMEOUT_MS = 30_000;

export async function verifyPackagedCliInput(
  binary: string,
  stack: TerminalTestStack,
  sessionId: string,
  marker: string,
): Promise<void> {
  const cliHome = mkdtempSync(join(tmpdir(), "roost-packaged-cli-"));
  try {
    const keyDirectory = join(cliHome, ".roost");
    mkdirSync(keyDirectory, { recursive: true, mode: 0o700 });
    copyFileSync(stack.apiKeyPath, join(keyDirectory, "cli-key"));
    const environment = isolatedCliEnvironment(cliHome, stack.baseUrl);

    const listed = await runPackagedCli(binary, ["api", "sessions", "--json"], environment);
    assert.equal(listed.exitCode, 0, `packaged CLI session discovery failed: ${listed.stderr}`);
    const sessions = parseListedSessions(listed.stdout);
    assert.ok(sessions.some((session) => session.id === sessionId), "packaged CLI did not discover the live session");

    const command = `printf '%s\\n' ${marker}`;
    const input = await runPackagedCli(
      binary,
      ["api", "input", sessionId, "--stdin", "--enter"],
      environment,
      new TextEncoder().encode(command),
    );
    assert.equal(input.exitCode, 0, `packaged CLI input failed: ${input.stderr}`);
    assert.equal(input.stdout, '{"ok":true,"accepted":true}\n', "packaged CLI did not accept terminal input");
  } finally {
    rmSync(cliHome, { recursive: true, force: true });
  }
}

async function runPackagedCli(
  binary: string,
  argumentsList: readonly string[],
  environment: NodeJS.ProcessEnv,
  stdin?: Uint8Array,
): Promise<{ exitCode: number | null; stdout: string; stderr: string }> {
  const child = Bun.spawn({
    cmd: [binary, ...argumentsList],
    cwd: process.cwd(),
    env: environment,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  });
  if (stdin) child.stdin.write(stdin);
  child.stdin.end();
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    waitForPackagedCliExit(child),
  ]);
  return { exitCode, stdout, stderr };
}

function isolatedCliEnvironment(home: string, coordinatorUrl: string): NodeJS.ProcessEnv {
  const inherited = Object.fromEntries(
    Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
  );
  return { ...inherited, HOME: home, ROOST_COORD_URL: coordinatorUrl };
}

async function waitForPackagedCliExit(child: Bun.Subprocess): Promise<number | null> {
  let timedOut = false;
  const deadline = setTimeout(() => {
    timedOut = true;
    try { child.kill("SIGKILL"); } catch { /* process exited at the deadline edge */ }
  }, CLI_TIMEOUT_MS);
  try {
    const exitCode = await child.exited;
    if (timedOut) throw new Error(`packaged CLI timed out after ${CLI_TIMEOUT_MS}ms`);
    return exitCode;
  } finally {
    clearTimeout(deadline);
  }
}

function parseListedSessions(stdout: string): Array<{ id: string }> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch (error) {
    throw new Error(`packaged CLI emitted invalid session JSON: ${String(error)}`);
  }
  if (!Array.isArray(parsed) || parsed.some((session) => !isSessionRecord(session))) {
    throw new Error("packaged CLI emitted an invalid session list");
  }
  return parsed;
}

function isSessionRecord(value: unknown): value is { id: string } {
  return typeof value === "object" && value !== null
    && "id" in value && typeof value.id === "string";
}
