import { createHash, randomUUID } from "node:crypto";
import { copyFile, lstat, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { sha256, sourceCommand } from "./source-command";

export interface NativeIdentity {
  package: string;
  version: string;
  integrity: string;
  loadedVersion: string;
  loadedPath: string;
  binaries: { path: string; sha256: string }[];
}

interface RegistryPackage {
  name: string;
  version: string;
  os: string[];
  cpu: string[];
  dist: { tarball: string; integrity: string };
}

async function fetchRequired(url: string): Promise<Response> {
  const response = await fetch(url, { redirect: "error" });
  if (!response.ok) {
    throw new Error(`Published OMP native binding prerequisite unavailable: ${url} returned HTTP ${response.status}. An exact matching published binding is required; no local/global addon or native rebuild is substituted.`);
  }
  return response;
}

async function provisionPublishedBinding(
  repoRoot: string,
  root: string,
  env: Record<string, string>,
  packageName: string,
  version: string,
  tag: string,
): Promise<Pick<NativeIdentity, "integrity" | "binaries">> {
  const metadata = await (await fetchRequired(`https://registry.npmjs.org/${encodeURIComponent(packageName)}/${version}`)).json() as RegistryPackage;
  if (metadata.name !== packageName || metadata.version !== version
    || !metadata.os.includes(process.platform) || !metadata.cpu.includes(process.arch)
    || !metadata.dist.integrity.startsWith("sha512-")) {
    throw new Error(`Published native binding metadata does not match ${packageName}@${version}.`);
  }
  const archiveUrl = new URL(metadata.dist.tarball);
  if (archiveUrl.protocol !== "https:" || archiveUrl.hostname !== "registry.npmjs.org") {
    throw new Error(`Refusing unexpected native binding archive origin: ${archiveUrl.origin}`);
  }
  const bytes = Buffer.from(await (await fetchRequired(archiveUrl.href)).arrayBuffer());
  const integrity = `sha512-${createHash("sha512").update(bytes).digest("base64")}`;
  if (integrity !== metadata.dist.integrity) throw new Error(`Native binding integrity mismatch for ${packageName}@${version}.`);
  const temporary = join(repoRoot, "target/omp-parity", `native-download-${randomUUID()}`);
  await mkdir(temporary, { recursive: true });
  try {
    const archive = join(temporary, "package.tgz");
    await writeFile(archive, bytes);
    const members = (await sourceCommand(["tar", "-tzf", archive], temporary, env)).split("\n");
    const names = process.arch === "x64"
      ? [`pi_natives.${tag}-baseline.node`, `pi_natives.${tag}-modern.node`, `pi_natives.${tag}.node`]
      : [`pi_natives.${tag}.node`];
    const selected = names.filter(name => members.includes(`package/${name}`));
    if (selected.length === 0) throw new Error(`${packageName}@${version} contains no published ${tag} addon.`);
    await sourceCommand(["tar", "-xzf", archive, "-C", temporary, "package/package.json", ...selected.map(name => `package/${name}`)], temporary, env);
    const manifestFile = join(temporary, "package/package.json");
    if (!(await lstat(manifestFile)).isFile()) throw new Error("Native package manifest is not a regular file.");
    const manifest = JSON.parse(await readFile(manifestFile, "utf8")) as { name: string; version: string };
    if (manifest.name !== packageName || manifest.version !== version) throw new Error("Native archive manifest does not match its registry identity.");
    const binaries: NativeIdentity["binaries"] = [];
    for (const name of selected) {
      const source = join(temporary, "package", name);
      if (!(await lstat(source)).isFile()) throw new Error(`Native addon is not a regular file: ${name}`);
      const digest = sha256(await readFile(source));
      const destination = join(root, "packages/natives/native", name);
      try {
        const existing = await lstat(destination);
        if (!existing.isFile() || sha256(await readFile(destination)) !== digest) {
          throw new Error(`Refusing stale native payload at ${destination}; preserve it and prepare a fresh owned checkout.`);
        }
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
        await copyFile(source, destination);
      }
      binaries.push({ path: relative(root, destination), sha256: digest });
    }
    return { integrity, binaries };
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function assertCachedBinding(root: string, native: NativeIdentity, packageName: string, version: string, tag: string): Promise<void> {
  if (native.package !== packageName || native.version !== version || native.loadedVersion !== version
    || !native.integrity.startsWith("sha512-") || native.binaries.length === 0) {
    throw new Error(`Owned native binding identity does not match ${packageName}@${version}.`);
  }
  const allowed = new Set([`pi_natives.${tag}.node`, `pi_natives.${tag}-baseline.node`, `pi_natives.${tag}-modern.node`]);
  for (const binary of native.binaries) {
    if (dirname(binary.path) !== join("packages", "natives", "native") || !allowed.has(basename(binary.path))) {
      throw new Error(`Refusing foreign native binding path: ${binary.path}`);
    }
    const file = join(root, binary.path);
    if (!(await lstat(file)).isFile() || sha256(await readFile(file)) !== binary.sha256) {
      throw new Error(`Owned native binding changed at ${file}. Nothing was replaced.`);
    }
  }
}

export async function installNativeBinding(
  repoRoot: string,
  root: string,
  env: Record<string, string>,
  existing?: NativeIdentity,
): Promise<NativeIdentity> {
  const tag = `${process.platform}-${process.arch}`;
  if (!["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"].includes(tag)) {
    throw new Error(`OMP parity requires a supported published native binding; ${tag} is not supported by this runner.`);
  }
  const manifest = JSON.parse(await readFile(join(root, "packages/natives/package.json"), "utf8")) as { version: string };
  const version = manifest.version;
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`OMP source native version is not an exact release: ${version}`);
  const packageName = `@oh-my-pi/pi-natives-${tag}`;
  if (existing) await assertCachedBinding(root, existing, packageName, version, tag);
  const payload = existing ?? await provisionPublishedBinding(repoRoot, root, env, packageName, version, tag);
  const loaderUrl = pathToFileURL(join(root, "packages/natives/native/loader-state.js")).href;
  const code = `import { loadNative, nativeAddonStatus } from ${JSON.stringify(loaderUrl)}; loadNative(); console.log(JSON.stringify(nativeAddonStatus()));`;
  const status = JSON.parse(await sourceCommand([process.execPath, "--eval", code], root, env)) as {
    path: string; version: string | null; packageVersion: string; stale: boolean;
  };
  const loadedPath = relative(root, resolve(status.path));
  if (status.version !== version || status.packageVersion !== version || status.stale
    || !payload.binaries.some(binary => binary.path === loadedPath)) {
    throw new Error(`Pinned OMP native loader loaded ${status.path} reporting ${status.version}, not owned ${packageName}@${version}.`);
  }
  return { package: packageName, version, integrity: payload.integrity, binaries: payload.binaries, loadedVersion: status.version, loadedPath };
}
