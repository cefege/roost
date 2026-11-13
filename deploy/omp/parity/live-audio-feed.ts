import { constants } from "node:fs";
import { lstat, open, readFile, realpath } from "node:fs/promises";
import type { FileHandle } from "node:fs/promises";
import { dirname, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { readProfile } from "./profile-manifest";

/** Real child-process entrypoint; the local live service owns its lifetime. */
export async function feedControlledLiveAudio(fifoPath: string, wavPath: string): Promise<void> {
  if (process.platform !== "linux") throw new Error("Controlled live audio feeder requires Linux");
  const root = await realpath(dirname(fifoPath));
  const home = dirname(root);
  const profile = await readProfile(dirname(home));
  if (await realpath(profile.home) !== home || root !== join(home, ".parity") || fifoPath !== join(root, "live-capture.pcm") || wavPath !== join(root, "live-capture.wav")) throw new Error("Audio feeder requires its owned fixture paths");
  const fifo = await lstat(fifoPath);
  const wavInfo = await lstat(wavPath);
  if (!fifo.isFIFO() || fifo.isSymbolicLink() || (fifo.mode & 0o777) !== 0o600 || !wavInfo.isFile() || wavInfo.isSymbolicLink()) throw new Error("Unexpected controlled audio files");
  const wav = await readFile(wavPath);
  if (wav.length < 684 || wav.toString("ascii", 0, 4) !== "RIFF" || wav.toString("ascii", 8, 16) !== "WAVEfmt " || wav.readUInt32LE(16) !== 16 || wav.readUInt16LE(20) !== 1 || wav.readUInt16LE(22) !== 1 || wav.readUInt32LE(24) !== 16000 || wav.readUInt16LE(34) !== 16 || wav.toString("ascii", 36, 40) !== "data" || wav.readUInt32LE(40) !== wav.length - 44) throw new Error("Audio feeder requires owned 16kHz mono signed16le WAV");
  const signedPcm = wav.subarray(44);
  if (!signedPcm.some(byte => byte !== 0)) throw new Error("Controlled audio must be nonzero");
  const pcm = Buffer.alloc((signedPcm.length / 2) * 4);
  for (let index = 0; index < signedPcm.length / 2; index++) pcm.writeFloatLE(signedPcm.readInt16LE(index * 2) / 32768, index * 4);
  let stopped = false;
  const stop = (): void => { stopped = true; };
  process.on("SIGTERM", stop); process.on("SIGINT", stop); process.on("SIGPIPE", stop);
  const deadline = performance.now() + 120_000;
  let output: FileHandle | undefined;
  try {
    while (!stopped && performance.now() < deadline) {
      try { output = await open(fifoPath, constants.O_WRONLY | constants.O_NONBLOCK | constants.O_NOFOLLOW); break; }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENXIO") throw error;
        await delay(20);
      }
    }
    if (!output) { if (!stopped) throw new Error("Controlled audio FIFO reader did not open before deadline"); return; }
    let offset = 0;
    while (!stopped && performance.now() < deadline) {
      const frame = pcm.subarray(offset, Math.min(offset + 1280, pcm.length));
      let written = 0;
      while (!stopped && written < frame.length && performance.now() < deadline) {
        try { written += (await output.write(frame, written, frame.length - written)).bytesWritten; }
        catch (error) {
          const code = (error as NodeJS.ErrnoException).code;
          if (code === "EPIPE") return;
          if (code !== "EAGAIN" && code !== "EWOULDBLOCK" && code !== "EINTR") throw error;
          await delay(5);
        }
      }
      offset = (offset + frame.length) % pcm.length;
      await delay(20);
    }
    if (!stopped) throw new Error("Controlled audio feeder exceeded its bounded lifetime");
  } finally {
    await output?.close();
    process.off("SIGTERM", stop); process.off("SIGINT", stop); process.off("SIGPIPE", stop);
  }
}

if (import.meta.main) {
  const [fifo, wav] = process.argv.slice(2);
  if (!fifo || !wav || process.argv.length !== 4) throw new Error("Expected owned FIFO and WAV paths");
  await feedControlledLiveAudio(fifo, wav);
}
