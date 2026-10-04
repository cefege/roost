// Drives the `roost api` CLI contract through the hermetic coordinator, worker, and real PTY.
// The stack client creates and observes the shell; the CLI must discover and write to it.
// No browser echo or mocked RPC can satisfy this exact-byte terminal bridge proof.

import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadWorkerKey, mintJwt } from "../support/worker-key.ts";
import { expect, test } from "./fixtures.ts";
import { resolveSmokeStackExecutables } from "./stack-executables.ts";

async function runCli(
  args: string[],
  environment: Record<string, string>,
  stdin?: Uint8Array,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const cli = Bun.spawn([resolveSmokeStackExecutables().coordExecutable, "api", ...args], {
    cwd: join(import.meta.dir, "..", ".."),
    env: environment,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  });
  if (stdin) cli.stdin.write(stdin);
  cli.stdin.end();
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(cli.stdout).text(),
    new Response(cli.stderr).text(),
    cli.exited,
  ]);
  return { exitCode, stdout, stderr };
}

test("CLI sessions JSON discovers a PTY and stdin input paints its marker", async ({ stack }) => {
  const cliHome = await mkdtemp(join(tmpdir(), "roost-cli-bridge-"));
  let sessionId: string | undefined;
  try {
    // `roost api` presents ROOST_CLI_TOKEN as its bearer and dials
    // ROOST_COORD_URL; the token is a JWT signed by the key the stack already
    // authorized, and the empty HOME keeps the operator's own install out of it.
    const environment = {
      ...Object.fromEntries(
        Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
      ),
      HOME: cliHome,
      ROOST_COORD_URL: stack.baseUrl,
      ROOST_CLI_TOKEN: await mintJwt(await loadWorkerKey(stack.apiKeyPath), "roost-coordinator"),
    } as Record<string, string>;
    sessionId = (await stack.client.sessionsSpawn({
      workerFp: stack.workerFp,
      kind: "shell",
      folder: "/tmp",
    })).sessionId;
    await expect.poll(async () => {
      const sessions = await stack.client.sessionsList({ status: "all" });
      return sessions.sessions.some((session) => session.id === sessionId && session.status === "open");
    }).toBe(true);

    const listed = await runCli(["sessions", "--json"], environment);
    expect(listed.exitCode, listed.stderr).toBe(0);
    const discovered = JSON.parse(listed.stdout) as Array<{ id: string }>;
    const target = discovered.find((session) => session.id === sessionId);
    expect(target).toBeTruthy();

    const marker = `CLI_BRIDGE_${crypto.randomUUID().replaceAll("-", "")}`;
    const input = await runCli(
      ["input", target!.id, "--stdin", "--enter"],
      environment,
      new TextEncoder().encode(`printf '%s\\n' ${marker}; seq 1 128`),
    );
    expect(input.exitCode, input.stderr).toBe(0);
    expect(input.stdout).toBe('{"ok":true,"accepted":true}\n');

    await expect.poll(async () => {
      const cells = await stack.client.sessionsGetScrollbackCells({
        sessionId: target!.id,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 200,
      });
      return cells.rows.some((row) => row.spans.map((span) => span.text).join("").trim() === marker);
    }).toBe(true);
  } finally {
    if (sessionId) await stack.client.sessionsKill({ sessionId }).catch(() => undefined);
    await rm(cliHome, { recursive: true, force: true });
  }
});
