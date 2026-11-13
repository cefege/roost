// Owns the paced ALSA FIFO writer child for one Linux live fixture leg.
// live-fixture-service starts it before native capture; no reader means a blocked child,
// not a blocked fixture server. Only its owned FIFO/WAV paths reach the child.

import { lstat } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export interface LiveFixtureFeeder {
  fifo: string;
  close(): Promise<void>;
}

export async function startLiveFixtureFeeder(
  home: string, wav: string, observe: (value: Record<string, unknown>) => void,
): Promise<LiveFixtureFeeder> {
  const fifo = join(home, ".parity", "live-capture.pcm");
  const metadata = await lstat(fifo);
  if (!metadata.isFIFO() || metadata.isSymbolicLink()) throw new Error("Live capture requires an owned ALSA PCM FIFO");
  const child = Bun.spawn([
    process.execPath, fileURLToPath(new URL("./live-audio-feed.ts", import.meta.url)), fifo, wav,
  ], { env: { HOME: home, TMPDIR: join(home, ".parity") }, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  let stopping = false;
  let closing: Promise<void> | undefined;
  const captureOutput = async (stream: ReadableStream<Uint8Array>): Promise<string> => {
    const reader = stream.getReader();
    const chunks: Uint8Array[] = [];
    let bytes = 0;
    try {
      while (true) {
        const next = await reader.read();
        if (next.done) return Buffer.concat(chunks).toString("utf8");
        bytes += next.value.byteLength;
        if (bytes > 64 * 1024) {
          child.kill("SIGKILL");
          throw new Error("Live audio feeder output exceeded its bound");
        }
        chunks.push(next.value);
      }
    } finally { await reader.cancel(); reader.releaseLock(); }
  };
  const completed = Promise.all([child.exited, captureOutput(child.stdout), captureOutput(child.stderr)]).then(([exitCode, stdout, stderr]) => {
    observe({ kind: "feeder-exit", exitCode, stdout, stderr, stopping });
  });
  void completed.catch(error => observe({ kind: "feeder-failure", message: String(error) }));
  return {
    fifo,
    close() {
      closing ??= (async () => {
        stopping = true;
        if (child.exitCode === null) child.kill("SIGTERM");
        const killed = setTimeout(() => { if (child.exitCode === null) child.kill("SIGKILL"); }, 1_000);
        const expiry = Promise.withResolvers<never>();
        const timeout = setTimeout(() => expiry.reject(new Error("Live audio feeder did not exit")), 3_000);
        try { await Promise.race([completed, expiry.promise]); }
        finally { clearTimeout(killed); clearTimeout(timeout); }
      })();
      return closing;
    },
  };
}
