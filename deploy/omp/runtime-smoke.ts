import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { createHome, isolatedEnvironment } from "./parity/runtime";

export async function smokeRuntime(binary: string): Promise<void> {
  const home = await mkdtemp(join(tmpdir(), "roost-omp-smoke-"));
  try {
    await createHome(home);
    const workspace = join(home, `ws-${crypto.randomUUID().slice(0, 6)}`);
    await mkdir(workspace);
    const child = Bun.spawn([binary, "--cwd", workspace, "acp", "--approval-mode", "yolo"], {
      // ACP's transport starts outside the workspace; the CLI must bind native UI too.
      cwd: join(home, "tmp"),
      env: isolatedEnvironment(home, {
        APPDATA: join(home, "appdata"),
        LOCALAPPDATA: join(home, "localappdata"),
      }),
      stdin: "pipe", stdout: "pipe", stderr: "pipe",
    });
    const stderr = new Response(child.stderr).text();
    const request = (id: number, method: string, params: object) => {
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
      child.stdin.flush();
    };
    let timer: NodeJS.Timeout | undefined;
    let failure: unknown;
    try {
      request(1, "initialize", {
        protocolVersion: 1,
        clientCapabilities: { _meta: { commandUi: { version: 1, protocol: "tsp" } } },
        clientInfo: { name: "roost-runtime-proof", version: "1" },
      });
      const response = (async () => {
        const reader = child.stdout.pipeThrough(new TextDecoderStream()).getReader();
        let pending = "";
        let opened = false;
        let renderedWorkspace = false;
        for (;;) {
          const next = await reader.read();
          if (next.done) throw new Error("OMP closed stdout before its native workspace rendered.");
          pending += next.value;
          let newline: number;
          while ((newline = pending.indexOf("\n")) !== -1) {
            const line = pending.slice(0, newline).trim();
            pending = pending.slice(newline + 1);
            if (!line) continue;
            const message = JSON.parse(line);
            if (message.error) throw new Error(`OMP request failed: ${JSON.stringify(message.error)}`);
            if (message.id === 1) {
              const capability = message.result?.agentCapabilities?._meta?.commandUi;
              if (capability?.version !== 1 || capability?.protocol !== "tsp") {
                throw new Error(`OMP returned an incompatible commandUi capability: ${JSON.stringify(capability)}`);
              }
              request(2, "session/new", { cwd: workspace, mcpServers: [] });
            }
            if (message.id === 2) {
              if (typeof message.result?.sessionId !== "string") throw new Error("OMP did not create a session.");
              request(3, "_omp/ui/open", {
                sessionId: message.result.sessionId,
                transportId: "release-runtime-proof",
                cols: 160, rows: 50, dark: true, reduceMotion: true,
              });
            }
            if (message.id === 3) {
              if (message.result?.version !== 1) throw new Error("OMP did not open its semantic UI.");
              opened = true;
            }
            if (message.method === "_omp/ui/message"
              && JSON.stringify(message.params?.message?.body)?.includes(basename(workspace))) {
              renderedWorkspace = true;
            }
            if (opened && renderedWorkspace) return;
          }
        }
      })();
      const timeout = Promise.withResolvers<never>();
      timer = setTimeout(() => timeout.reject(new Error("OMP native workspace UI timed out after 60 seconds.")), 60_000);
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
