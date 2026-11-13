import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, copyFile, lstat, mkdir, writeFile } from "node:fs/promises";
import { delimiter, dirname, join, resolve } from "node:path";
import { sourceCommand } from "./parity/source-command";
import { smokeRuntime } from "./runtime-smoke";
import { prepareSource, sourceEnvironment } from "./parity/source";

const assets: Record<string, string> = {
  "darwin-arm64": "roost-omp",
  "darwin-x64": "roost-omp-darwin-x64",
  "linux-x64": "roost-omp-linux-x64",
  "linux-arm64": "roost-omp-linux-arm64",
  "win32-x64": "roost-omp-windows-x64.exe",
};

async function buildRuntime(): Promise<void> {
  const platform = `${process.platform}-${process.arch}`;
  const asset = assets[platform];
  if (!asset) throw new Error(`No Roost OMP runtime asset for ${platform}.`);
  const repoRoot = resolve(import.meta.dir, "../..");
  const source = await prepareSource(repoRoot);
  const env = await sourceEnvironment(repoRoot);
  env.PATH = `${dirname(process.execPath)}${delimiter}${env.PATH}`;
  const packageRoot = join(source.root, "packages/coding-agent");
  await sourceCommand([process.execPath, "scripts/build-binary.ts"], packageRoot, env);

  const candidates = (process.platform === "win32" ? ["omp", "omp.exe"] : ["omp"])
    .map(name => join(packageRoot, "dist", name));
  const binaries: string[] = [];
  for (const candidate of candidates) {
    try {
      if (!(await lstat(candidate)).isFile()) throw new Error(`OMP output is not a regular file: ${candidate}`);
      binaries.push(candidate);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
  }
  if (binaries.length !== 1) throw new Error(`Expected exactly one OMP executable in ${join(packageRoot, "dist")}; found ${binaries.length}.`);
  const binary = binaries[0];
  const output = join(repoRoot, "target/omp-runtime");
  await mkdir(output, { recursive: true });
  const destination = join(output, asset);
  await copyFile(binary, destination);
  if (process.platform !== "win32") await chmod(destination, 0o755);
  await smokeRuntime(destination);
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(destination)) hash.update(chunk);
  await writeFile(`${destination}.sha256`, `${hash.digest("hex")}  ${asset}\n`);
  process.stdout.write(`${destination}\n`);
}

await buildRuntime();
