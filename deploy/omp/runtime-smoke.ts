import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

export async function smokeRuntime(binary: string): Promise<void> {
  const home = await mkdtemp(join(tmpdir(), "roost-omp-smoke-"));
  try {
    const env: Record<string, string> = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (value !== undefined && !/^(PI_|OMP_|ROOST_|XDG_)/i.test(key)) env[key] = value;
    }
    Object.assign(env, {
      HOME: home,
      USERPROFILE: home,
      APPDATA: join(home, "appdata"),
      LOCALAPPDATA: join(home, "localappdata"),
      XDG_CACHE_HOME: join(home, "cache"),
      XDG_CONFIG_HOME: join(home, "config"),
      XDG_DATA_HOME: join(home, "data"),
      PI_CODING_AGENT_DIR: join(home, "agent"),
      TMPDIR: home,
      TMP: home,
      TEMP: home,
    });
    const child = Bun.spawn([binary, "acp", "--approval-mode", "yolo"], {
      cwd: home, env, stdin: "pipe", stdout: "pipe", stderr: "pipe",
    });
    const stderr = new Response(child.stderr).text();
    let timer: NodeJS.Timeout | undefined;
    let failure: unknown;
    try {
      child.stdin.write(`${JSON.stringify({
        jsonrpc: "2.0", id: 1, method: "initialize",
        params: {
          protocolVersion: 1,
          clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: true },
          clientInfo: { name: "roost-runtime-proof", version: "1" },
        },
      })}\n`);
      child.stdin.flush();
      const response = (async () => {
        const reader = child.stdout.pipeThrough(new TextDecoderStream()).getReader();
        let pending = "";
        for (;;) {
          const next = await reader.read();
          if (next.done) throw new Error("OMP closed stdout before initialize completed.");
          pending += next.value;
          let newline: number;
          while ((newline = pending.indexOf("\n")) !== -1) {
            const line = pending.slice(0, newline).trim();
            pending = pending.slice(newline + 1);
            if (!line) continue;
            const message = JSON.parse(line);
            if (message.id !== 1) continue;
            if (message.error) throw new Error(`OMP initialize returned an error: ${JSON.stringify(message.error)}`);
            const capability = message.result?.agentCapabilities?._meta?.commandUi;
            if (capability?.version !== 1 || capability?.protocol !== "tsp") {
              throw new Error(`OMP initialize returned an incompatible commandUi capability: ${JSON.stringify(capability)}`);
            }
            return;
          }
        }
      })();
      const timeout = Promise.withResolvers<never>();
      timer = setTimeout(() => timeout.reject(new Error("OMP initialize timed out after 60 seconds.")), 60_000);
      await Promise.race([response, timeout.promise]);
    } catch (error) {
      failure = error;
    } finally {
      clearTimeout(timer);
      child.stdin.end();
      child.kill();
      await child.exited;
    }
    if (failure) throw new Error(`Standalone OMP smoke failed: ${String(failure)}\n${await stderr}`);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
}

if (import.meta.main) {
  const binary = process.argv[2];
  if (!binary || process.argv.length !== 3) throw new Error("Usage: bun deploy/omp/runtime-smoke.ts <binary>");
  await smokeRuntime(resolve(binary));
}
