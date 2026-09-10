// Drives the compiled CLI contract through the hermetic coordinator, worker, and real PTY.
// The stack client creates and observes the shell; the CLI must discover and write to it.
// No browser echo or mocked RPC can satisfy this exact-byte terminal bridge proof.

import { copyFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { devNull, tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "./fixtures.ts";

const bunExecutable = process.env.ROOST_TEST_BUN ?? Bun.which("bun") ?? (() => {
  throw new Error("terminal CLI smoke requires Bun");
})();

async function runCli(
  args: string[],
  environment: Record<string, string>,
  stdin?: Uint8Array,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const cli = Bun.spawn([bunExecutable, `--env-file=${devNull}`, "apps/roost-cli/src/main.ts", "api", ...args], {
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
  const cliKeyPath = join(cliHome, ".roost", "cli-key");
  let sessionId: string | undefined;
  try {
    await mkdir(join(cliHome, ".roost"), { recursive: true, mode: 0o700 });
    await copyFile(stack.apiKeyPath, cliKeyPath);
    const environment = {
      ...Object.fromEntries(
        Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
      ),
      HOME: cliHome,
      ROOST_COORD_URL: stack.baseUrl,
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
