#!/usr/bin/env bun
// Build the self-contained `roost` binaries:
//   vite build (SPA) → gen-embed (bake SPA + migrations) → bun build --compile
//   → restore the empty embed stubs so the working tree stays clean.
// The output binaries embed the Bun runtime, so they run with no Bun installed.
//
// Default: the full release matrix, with `dist/roost` copied from the explicit
// Darwin arm64 target for deterministic compatibility. `--host-only` builds
// only the host-native `dist/roost`; `--windows-only` builds the Windows PE
// consumed by scripts/windows/package-windows.ps1.
import { $ } from "bun";
import { copyFile } from "node:fs/promises";

const OUT = "dist/roost";
// Asset names must match releaseAssetName() in apps/roost-cli/src/update.ts and
// the installers. The unsuffixed compatibility asset is copied from the
// explicit Darwin arm64 output after every full matrix build.
const DARWIN_ARM64_OUT = "dist/roost-darwin-arm64";
const WINDOWS_X64_OUT = "dist/roost-windows-x64.exe";
const TARGETS = [
  { target: "bun-darwin-arm64", out: DARWIN_ARM64_OUT },
  { target: "bun-darwin-x64", out: "dist/roost-darwin-x64" },
  { target: "bun-linux-x64", out: "dist/roost-linux-x64" },
  { target: "bun-linux-arm64", out: "dist/roost-linux-arm64" },
  { target: "bun-windows-x64-baseline", out: WINDOWS_X64_OUT },
];
const hostOnly = process.argv.includes("--host-only");
const windowsOnly = process.argv.includes("--windows-only");
const unknownArgs = process.argv.slice(2).filter((arg) => !["--host-only", "--windows-only"].includes(arg));
if (unknownArgs.length > 0) throw new Error(`unknown argument(s): ${unknownArgs.join(", ")}`);
if (hostOnly && windowsOnly) throw new Error("--host-only and --windows-only are mutually exclusive");

// Stamp semantic artifact version and the full immutable commit separately.
// The version is human/release-facing; service health and fleet convergence
// use the full SHA so binary replacement cannot inherit stale service env.
const pkg = await Bun.file("package.json").json();
const gitSha = (await $`git rev-parse HEAD`.nothrow().quiet().text()).trim();
if (!/^[0-9a-f]{40,64}$/i.test(gitSha)) {
  throw new Error("cannot build release artifacts without a full immutable Git commit SHA");
}
const dirty = (await $`git status --porcelain`.nothrow().quiet().text()).trim();
if (dirty) {
  throw new Error("cannot build release artifacts from a dirty working tree");
}
const VERSION = gitSha ? `${pkg.version}+${gitSha.slice(0, 8)}` : `${pkg.version}`;

console.log(">> vite build (apps/web)");
await $`bun x vite build`.env({ ...process.env, ROOST_GIT_SHA: gitSha }).cwd("apps/web");

try {
  console.log(">> gen-embed (baking SPA + migrations)");
  await $`bun scripts/gen-embed.ts`;

  const defineArgs = [
    "--define", `__ROOST_VERSION__=${JSON.stringify(VERSION)}`,
    "--define", `__ROOST_GIT_SHA__=${JSON.stringify(gitSha)}`,
  ];
  if (hostOnly) {
    console.log(`>> bun build --compile → ${OUT} (host, version ${VERSION})`);
    await $`bun build --compile ${defineArgs} apps/roost-cli/src/main.ts --outfile ${OUT}`;
  } else {
    const targets = windowsOnly
      ? TARGETS.filter((candidate) => candidate.out === WINDOWS_X64_OUT)
      : TARGETS;
    for (const t of targets) {
      console.log(`>> bun build --compile --target=${t.target} → ${t.out}`);
      await $`bun build --compile --target=${t.target} ${defineArgs} apps/roost-cli/src/main.ts --outfile ${t.out}`;
    }
    if (!windowsOnly) {
      console.log(`>> copy ${DARWIN_ARM64_OUT} → ${OUT}`);
      await copyFile(DARWIN_ARM64_OUT, OUT);
    }
  }
} finally {
  console.log(">> restore embed stubs");
  await $`bun scripts/gen-embed.ts --stub`;
}

const built = hostOnly
  ? OUT
  : windowsOnly
    ? WINDOWS_X64_OUT
    : `${OUT} + ${TARGETS.map((target) => target.out).join(", ")}`;
console.log(`\n✓ built ${built}`);
