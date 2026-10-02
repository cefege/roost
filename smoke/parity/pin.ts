// Builds the Rust oracle artifacts and pins them at <repo>/.smoke-pin with a manifest naming them.
// run.ts `build` calls buildAndPinArtifacts; playwright.ts reads the pin through readPinManifest.
// The pin sits outside target/, so stack-rust-binaries.ts uses it as given instead of rebuilding,
// and every file is replaced by rename, so a stack still running the old inode is never disturbed.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
	chmodSync,
	copyFileSync,
	cpSync,
	existsSync,
	mkdirSync,
	readdirSync,
	readFileSync,
	renameSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { z } from "zod";

export const REPOSITORY_ROOT = resolve(import.meta.dir, "../..");
export const PIN_DIRECTORY = join(REPOSITORY_ROOT, ".smoke-pin");
const PIN_MANIFEST_PATH = join(PIN_DIRECTORY, "manifest.json");
const PINNED_WEB_DIRECTORY = join(PIN_DIRECTORY, "web");
const CLI_CARGO_MANIFEST = join(REPOSITORY_ROOT, "crates/roost-cli/Cargo.toml");
/** A `smoke = [...]` feature line in the roost-cli manifest; absent until the crate declares it. */
const CLI_SMOKE_FEATURE_LINE = /^smoke\s*=/m;
const SMOKE_FEATURE = "smoke";
const WEB_BUNDLE_FILE = /^roost-web[-_].*\.(js|wasm)$/;

/** What `.smoke-pin/manifest.json` records about the artifacts beside it. */
export const PinManifestSchema = z.object({
	gitSha: z.string(),
	// The working tree had uncommitted changes when the artifacts were built.
	dirty: z.boolean(),
	roostSha256: z.string(),
	keeperSha256: z.string(),
	webBundleFiles: z.array(z.string()),
	// Cargo features of `roost`; `smoke` arms the worker's peer-fault controls.
	features: z.array(z.string()),
	// Cargo features of the dx bundle; `smoke` installs `window.__smoke`.
	webFeatures: z.array(z.string()),
	// `smoke` is release optimisation without whole-program LTO, for the per-fix loop only.
	profile: z.enum(["release", "smoke"]).default("release"),
	builtAt: z.string(),
});
export type PinManifest = z.infer<typeof PinManifestSchema>;

export interface PinBuildOptions {
	/** Rebuild and re-pin the dx bundle; false keeps the pinned `web/` exactly as it is. */
	readonly buildWeb: boolean;
	/** The production shape: no smoke feature on any artifact. */
	readonly plain: boolean;
	/** Build with the `smoke` cargo profile: a one-crate change re-links instead of re-running LTO. */
	readonly fast: boolean;
}

/** A refusal the caller should report as a usage error (exit 2), not a failed run. */
export class ParityRefusal extends Error {
	constructor(message: string) {
		super(message);
		this.name = "ParityRefusal";
	}
}

export function currentGitSha(): string {
	return gitOutput(["rev-parse", "HEAD"]);
}

export function workingTreeDirty(): boolean {
	return gitOutput(["status", "--porcelain"]) !== "";
}

/** The pinned manifest, or `null` when nothing has been pinned in this checkout. */
export function readPinManifest(): PinManifest | null {
	if (!existsSync(PIN_MANIFEST_PATH)) return null;
	return PinManifestSchema.parse(JSON.parse(readFileSync(PIN_MANIFEST_PATH, "utf8")));
}

/**
 * The pin a run may drive: present, and built from HEAD unless the caller allows a stale one.
 * A run against artifacts from another commit proves that commit, not this one.
 */
export function requireCurrentPin(allowStale: boolean): PinManifest {
	const manifest = readPinManifest();
	if (manifest === null) {
		throw new ParityRefusal(`no pinned artifacts at ${PIN_MANIFEST_PATH}; run \`bun smoke/parity/run.ts build\` first`);
	}
	const head = currentGitSha();
	if (manifest.gitSha !== head && !allowStale) {
		throw new ParityRefusal(
			`the pin was built from ${manifest.gitSha} but HEAD is ${head}; rebuild, or pass --allow-stale to drive it anyway`,
		);
	}
	return manifest;
}

/** One line naming the artifacts a run drives; printed before every run. */
export function formatPinManifest(manifest: PinManifest): string {
	return [
		`pin ${manifest.gitSha.slice(0, 12)}${manifest.dirty ? "+dirty" : ""}`,
		`roost=${manifest.roostSha256.slice(0, 12)}`,
		`keeper=${manifest.keeperSha256.slice(0, 12)}`,
		`web=${manifest.webBundleFiles.join(",")}`,
		`features=[${manifest.features.join(",")}]`,
		`web-features=[${manifest.webFeatures.join(",")}]`,
		`profile=${manifest.profile}`,
		`built=${manifest.builtAt}`,
	].join(" ");
}

/**
 * Build `roost`, `roost-keeper` and (unless kept) the dx bundle, then pin all three.
 *
 * The roost-cli smoke feature is requested only once the crate declares it, so the same command
 * works before and after the worker's fault controls exist; the manifest records which it got.
 */
export function buildAndPinArtifacts(options: PinBuildOptions): PinManifest {
	const previous = readPinManifest();
	if (!options.buildWeb && (previous === null || !existsSync(PINNED_WEB_DIRECTORY))) {
		throw new ParityRefusal(`--no-web keeps the pinned bundle, and ${PINNED_WEB_DIRECTORY} has none yet`);
	}
	const gitSha = currentGitSha();
	const dirty = workingTreeDirty();
	const configuredTarget = process.env.CARGO_TARGET_DIR?.trim();
	const targetDirectory = configuredTarget
		? resolve(REPOSITORY_ROOT, configuredTarget)
		: join(REPOSITORY_ROOT, "target");

	const cliDeclaresSmoke = CLI_SMOKE_FEATURE_LINE.test(readFileSync(CLI_CARGO_MANIFEST, "utf8"));
	const features = !options.plain && cliDeclaresSmoke ? [SMOKE_FEATURE] : [];
	const profile = options.fast ? "smoke" : "release";
	runBuildStep("cargo", [
		"build", "--profile", profile, "-p", "roost-cli", "-p", "roost-keeper",
		...(features.length > 0 ? ["--features", "roost-cli/smoke"] : []),
	]);

	let webFeatures = previous?.webFeatures ?? [];
	if (options.buildWeb) {
		webFeatures = options.plain ? [] : [SMOKE_FEATURE];
		const publicDirectory = join(targetDirectory, "dx/roost-web/release/web/public");
		// dx never prunes old hashed bundles from public/, and a pinned directory holding three
		// generations of roost-web_bg-*.wasm cannot say which one the page will load.
		rmSync(publicDirectory, { recursive: true, force: true });
		// `--profile release` makes dx declare `release` with `inherits = "release"`, which cargo
		// rejects ("`inherits` must not be specified in root profile"): only `smoke` is named.
		runBuildStep("dx", [
			"build", "--release", ...(options.fast ? ["--profile", profile] : []), "-p", "roost-web", "--platform", "web",
			...(webFeatures.length > 0 ? ["--features", SMOKE_FEATURE] : []),
		]);
		if (!existsSync(join(publicDirectory, "index.html"))) {
			throw new Error(`dx build finished but ${publicDirectory}/index.html does not exist`);
		}
		mkdirSync(PIN_DIRECTORY, { recursive: true });
		pinDirectory(publicDirectory, PINNED_WEB_DIRECTORY);
	}

	mkdirSync(PIN_DIRECTORY, { recursive: true });
	const manifest: PinManifest = {
		gitSha,
		dirty,
		roostSha256: pinExecutable(join(targetDirectory, profile, "roost"), "roost"),
		keeperSha256: pinExecutable(join(targetDirectory, profile, "roost-keeper"), "roost-keeper"),
		webBundleFiles: webBundleFiles(),
		features,
		webFeatures,
		profile,
		builtAt: new Date().toISOString(),
	};
	writeFileSync(PIN_MANIFEST_PATH, `${JSON.stringify(manifest, null, 2)}\n`);
	return manifest;
}

/** Run one build command in the foreground; cargo's own output is the useful part of a failure. */
function runBuildStep(program: string, args: string[]): void {
	const executable = program === "cargo" ? (process.env.CARGO ?? "cargo") : program;
	console.log(`>> ${program} ${args.join(" ")}`);
	const result = spawnSync(executable, args, { cwd: REPOSITORY_ROOT, stdio: "inherit" });
	if (result.error) throw new Error(`${program} did not start: ${String(result.error)}`);
	if (result.status !== 0) throw new Error(`${program} ${args[0]} exited ${result.status ?? "by signal"}`);
}

/** Copy, never link: stack-rust-binaries.ts stats through a symlink back into target/. */
function pinExecutable(source: string, name: string): string {
	if (!existsSync(source)) throw new Error(`cargo build finished but ${source} does not exist`);
	const staged = join(PIN_DIRECTORY, `.${name}.staged`);
	copyFileSync(source, staged);
	chmodSync(staged, 0o755);
	renameSync(staged, join(PIN_DIRECTORY, name));
	return createHash("sha256").update(readFileSync(join(PIN_DIRECTORY, name))).digest("hex");
}

function pinDirectory(source: string, destination: string): void {
	const staged = `${destination}.staged`;
	rmSync(staged, { recursive: true, force: true });
	cpSync(source, staged, { recursive: true, dereference: true });
	rmSync(destination, { recursive: true, force: true });
	renameSync(staged, destination);
}

function webBundleFiles(): string[] {
	const assets = join(PINNED_WEB_DIRECTORY, "assets");
	if (!existsSync(assets)) return [];
	return readdirSync(assets).filter((name) => WEB_BUNDLE_FILE.test(name)).sort();
}

function gitOutput(args: string[]): string {
	const result = spawnSync("git", args, { cwd: REPOSITORY_ROOT, encoding: "utf8" });
	if (result.status !== 0) throw new Error(`git ${args.join(" ")} exited ${result.status}: ${result.stderr}`);
	return result.stdout.trim();
}
