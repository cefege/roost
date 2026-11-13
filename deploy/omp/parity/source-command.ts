import { createHash } from "node:crypto";

export function sha256(bytes: Uint8Array | string): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export async function sourceCommand(
  command: string[],
  cwd: string,
  env: Record<string, string>,
): Promise<string> {
  const child = Bun.spawn(command, { cwd, env, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, status] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (status !== 0) {
    throw new Error(`OMP source prerequisite failed (${status}): ${command.join(" ")}\n${stderr}\n${stdout}`);
  }
  if (stderr) process.stderr.write(stderr);
  return stdout.trim();
}
