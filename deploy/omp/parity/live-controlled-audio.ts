import { lstat, mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import type * as SourceDirs from "@omp-source/packages/utils/src/dirs";
import type * as SourceAuth from "@omp-source/packages/ai/src/auth-storage";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { BuiltinSeedContext } from "./builtin-seed";
import { assertOwned, readProfile } from "./profile-manifest";

async function ownedHome(context: BuiltinSeedContext): Promise<string> {
  if (process.platform !== "linux") throw new Error("Controlled positive live fixtures require Linux");
  const profile = await readProfile(context.workspace);
  const home = await realpath(context.home);
  if (home !== await realpath(profile.home) || resolve(context.sourceRoot) !== resolve(profile.sourceRoot)) throw new Error("Live fixture requires its owned profile home and source");
  return home;
}

export async function prepareControlledLiveAudio(context: BuiltinSeedContext): Promise<Record<string, string>> {
  const home = await ownedHome(context);
  const root = join(home, ".parity");
  await mkdir(root, { recursive: true, mode: 0o700 });
  assertOwned(home, await realpath(root));
  const sampleRate = 16000;
  const samples = sampleRate * 120;
  const pcm = Buffer.alloc(samples * 2);
  for (let index = 0; index < samples; index++) pcm.writeInt16LE(Math.round(640 * Math.sin(2 * Math.PI * 440 * index / sampleRate)), index * 2);
  const wav = Buffer.alloc(44 + pcm.length);
  wav.write("RIFF", 0); wav.writeUInt32LE(36 + pcm.length, 4); wav.write("WAVEfmt ", 8);
  wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(sampleRate, 24); wav.writeUInt32LE(sampleRate * 2, 28);
  wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34); wav.write("data", 36);
  wav.writeUInt32LE(pcm.length, 40); pcm.copy(wav, 44);
  const rawPath = join(root, "live-capture.pcm");
  const fifo = await lstat(rawPath).catch(error => { if (error.code === "ENOENT") return undefined; throw error; });
  if (fifo) {
    if (!fifo.isFIFO() || fifo.isSymbolicLink() || (fifo.mode & 0o777) !== 0o600) throw new Error("Unexpected controlled audio FIFO");
  } else await promisify(execFile)("mkfifo", ["-m", "600", rawPath]);
  const wavPath = join(root, "live-capture.wav");
  const alsaPath = join(root, "live-asound.conf");
  const pulsePath = join(root, "live-pulse.conf");
  for (const [path, content] of [[wavPath, wav], [alsaPath, `pcm.parity_null { type null }\npcm.parity_file { type file slave.pcm parity_null file "/dev/null" infile ${JSON.stringify(rawPath)} format "raw" }\npcm.parity_capture { type plug slave { pcm parity_file format FLOAT_LE channels 1 rate ${sampleRate} } }\npcm.!default { type asym capture.pcm parity_capture playback.pcm parity_null }\n`], [pulsePath, "autospawn = no\ndaemon-binary = /bin/false\n"]] as const) {
    const existing = await lstat(path).catch(error => { if (error.code === "ENOENT") return undefined; throw error; });
    if (existing) {
      if (!existing.isFile() || existing.isSymbolicLink() || !(await readFile(path)).equals(Buffer.from(content))) throw new Error(`Unexpected controlled audio file: ${path}`);
    } else await writeFile(path, content, { flag: "wx", mode: 0o600 });
  }
  // The published Linux backend tries PulseAudio before ALSA. Never let it discover a host server.
  return { ALSA_CONFIG_PATH: alsaPath, OMP_PARITY_LIVE_WAV: wavPath, OMP_PARITY_LIVE_PCM_FIFO: rawPath,
    PULSE_SERVER: `unix:${join(root, "live-no-pulse.sock")}`, PULSE_CLIENTCONFIG: pulsePath };
}

export async function seedControlledLiveAuth(context: BuiltinSeedContext): Promise<void> {
  const home = await ownedHome(context);
  if (await realpath(process.env.HOME ?? "/") !== home) throw new Error("Live auth seeding requires the isolated HOME process");
  if (!process.env.PI_CODING_AGENT_DIR) throw new Error("Live auth requires an explicit owned PI_CODING_AGENT_DIR");
  assertOwned(home, process.env.PI_CODING_AGENT_DIR);
  const { getAgentDbPath } = await context.load<typeof SourceDirs>("packages/utils/src/dirs.ts");
  const dbPath = resolve(getAgentDbPath());
  assertOwned(home, dbPath);
  await mkdir(join(dbPath, ".."), { recursive: true, mode: 0o700 });
  assertOwned(home, await realpath(join(dbPath, "..")));
  const existing = await lstat(dbPath).catch(error => { if (error.code === "ENOENT") return undefined; throw error; });
  if (existing && (!existing.isFile() || existing.isSymbolicLink())) throw new Error("Controlled live auth database must be an owned regular file");
  const { AuthStorage } = await context.load<typeof SourceAuth>("packages/ai/src/auth-storage.ts");
  const auth = await AuthStorage.create(dbPath);
  try {
    if (auth.credentials.hasOAuth("openai-codex")) throw new Error("Controlled live auth refuses preexisting Codex credentials");
    const identity = randomUUID();
    await auth.credentials.set("openai-codex", [{ type: "oauth", access: `parity-fixture-access-${identity}`,
      refresh: `parity-fixture-refresh-${identity}`, expires: Date.now() + 7 * 24 * 60 * 60 * 1000,
      accountId: `parity-${identity}`, email: "parity-live@example.invalid" }]);
  } finally { auth.close(); }
}
