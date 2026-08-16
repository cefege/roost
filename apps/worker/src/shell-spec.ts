import { mkdirSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, win32 } from "node:path";
import {
	assertNeverPlatform,
	isSupportedHostPlatform,
	supportedHostPlatform,
	type SupportedHostPlatform,
} from "@roost/shared/platform";
import { normalizeNativePath } from "@roost/shared/native-path";
import { psReadLineHistoryPath, withHistfile } from "./keeper/histfile.ts";

/**
 * Fully resolved, serializable shell launch contract. `argv` contains only
 * arguments; `executable` is always passed separately to the process boundary.
 * The keeper must execute this contract verbatim rather than applying shell
 * policy of its own.
 */
export interface ShellSpec {
	version: 1;
	platform: SupportedHostPlatform;
	executable: string;
	argv: string[];
	cwd: string;
	env: Record<string, string>;
}

export interface ResolveShellSpecOptions {
	cwd: string;
	sessionId: string;
	platform?: SupportedHostPlatform;
	/** Inherited service environment. Defaults to process.env. */
	environment?: NodeJS.ProcessEnv;
	/** Per-session variables, such as the agent report endpoint/capability. */
	envOverlay?: Record<string, string>;
}

const POWERSHELL_BOOTSTRAP = [
	"$roostUtf8 = [System.Text.UTF8Encoding]::new($false)",
	"try { [Console]::InputEncoding = $roostUtf8 } catch {}",
	"try { [Console]::OutputEncoding = $roostUtf8 } catch {}",
	"$global:OutputEncoding = $roostUtf8",
	"if ($env:ROOST_PSREADLINE_HISTORY -and (Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue)) {",
	"  Set-PSReadLineOption -HistorySavePath $env:ROOST_PSREADLINE_HISTORY -ErrorAction SilentlyContinue",
	"}",
	"function global:__RoostEmitOsc7 {",
	"  try {",
	"    $roostProviderPath = (Get-Location).ProviderPath",
	"    if ($roostProviderPath) {",
	"      $roostUri = ([System.Uri]$roostProviderPath).AbsoluteUri",
	'      [Console]::Write("`e]7;$roostUri`a")',
	"    }",
	"  } catch {}",
	"}",
	"if (-not (Test-Path variable:global:__RoostOriginalPrompt)) {",
	"  $global:__RoostOriginalPrompt = ${function:prompt}",
	"  function global:prompt {",
	"    $roostLastExitCode = $global:LASTEXITCODE",
	"    $roostPromptText = & $global:__RoostOriginalPrompt",
	"    $global:LASTEXITCODE = $roostLastExitCode",
	"    __RoostEmitOsc7",
	"    $roostPromptText",
	"  }",
	"}",
	"__RoostEmitOsc7",
].join("\n");

const WINDOWS_REQUIRED_ENV = [
	"SystemRoot",
	"COMSPEC",
	"PATHEXT",
	"TEMP",
	"USERPROFILE",
] as const;

function environmentValue(
	environment: NodeJS.ProcessEnv | Record<string, string>,
	name: string,
	caseInsensitive: boolean,
): string | undefined {
	if (!caseInsensitive) return environment[name];
	const folded = name.toUpperCase();
	for (const [key, value] of Object.entries(environment)) {
		if (key.toUpperCase() === folded) return value;
	}
	return undefined;
}

/** Keeper control credentials are worker/keeper-only and never enter a PTY. */
function isKeeperControlEnvironmentKey(key: string): boolean {
	return key.toUpperCase().startsWith("ROOST_KEEPER_");
}

function mergeEnvironment(
	platform: SupportedHostPlatform,
	base: NodeJS.ProcessEnv,
	...overlays: Array<Record<string, string>>
): Record<string, string> {
	if (platform === "darwin" || platform === "linux") {
		const result: Record<string, string> = {};
		for (const [key, value] of Object.entries(base)) {
			if (value !== undefined && !isKeeperControlEnvironmentKey(key)) result[key] = value;
		}
		for (const overlay of overlays) {
			for (const [key, value] of Object.entries(overlay)) {
				if (!isKeeperControlEnvironmentKey(key)) result[key] = value;
			}
		}
		return result;
	}
	if (platform === "win32") {
		const values = new Map<string, { key: string; value: string }>();
		const merge = (source: NodeJS.ProcessEnv | Record<string, string>): void => {
			for (const [key, value] of Object.entries(source)) {
				if (value === undefined || isKeeperControlEnvironmentKey(key)) continue;
				const folded = key.toUpperCase();
				const current = values.get(folded);
				values.set(folded, { key: current?.key ?? key, value });
			}
		};
		merge(base);
		for (const overlay of overlays) merge(overlay);
		const result: Record<string, string> = {};
		for (const { key, value } of values.values()) result[key] = value;
		const home = environmentValue(result, "HOME", true);
		const userProfile = environmentValue(result, "USERPROFILE", true);
		if (!home && userProfile) {
			for (const key of Object.keys(result)) {
				if (key.toUpperCase() === "HOME") delete result[key];
			}
			result.HOME = userProfile;
		}
		// Keep the service's canonical spelling for variables Windows process
		// creation relies on, even when an overlay used different casing.
		for (const name of WINDOWS_REQUIRED_ENV) {
			const value = environmentValue(result, name, true);
			if (value === undefined) continue;
			for (const key of Object.keys(result)) {
				if (key !== name && key.toUpperCase() === name.toUpperCase()) delete result[key];
			}
			result[name] = value;
		}
		return result;
	}
	return assertNeverPlatform(platform);
}

const POSIX_BOOTSTRAP_PATHS: Partial<Record<"bash" | "zsh", string>> = {};

function ensurePosixBootstrap(kind: "bash" | "zsh"): string {
	const existing = POSIX_BOOTSTRAP_PATHS[kind];
	if (existing) return existing;
	const root = tmpdir();
	const path = kind === "zsh"
		? join(root, "roost-zsh-noPROMPT_SP")
		: join(root, "roost-bash-osc7", "roost.bashrc");
	if (kind === "zsh") {
		mkdirSync(path, { recursive: true, mode: 0o700 });
		writeFileSync(join(path, ".zshrc"), [
			"# roost: disable PROMPT_SP so the SPA wterm doesn't see whitespace junk",
			"unsetopt PROMPT_SP PROMPT_CR 2>/dev/null",
			"PROMPT_EOL_MARK=''",
			"# Source the real user zshrc so theme/aliases/path still load",
			'if [ -f "$HOME/.zshrc" ]; then source "$HOME/.zshrc"; fi',
			'function roost_emit_osc7 { print -Pn "\\e]7;file://${HOST}${PWD}\\e\\\\" }',
			"autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook chpwd roost_emit_osc7",
			"roost_emit_osc7",
			"",
		].join("\n"), { mode: 0o600 });
		POSIX_BOOTSTRAP_PATHS.zsh = path;
		return path;
	}
	mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
	writeFileSync(path, [
		"# roost: source the user's real bashrc first so PATH/aliases still load",
		'if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi',
		'roost_emit_osc7() { printf \'\\033]7;file://%s%s\\033\\\\\' "${HOSTNAME}" "$PWD"; }',
		'PROMPT_COMMAND="roost_emit_osc7${PROMPT_COMMAND:+; $PROMPT_COMMAND}"',
		"roost_emit_osc7",
		"",
	].join("\n"), { mode: 0o600 });
	POSIX_BOOTSTRAP_PATHS.bash = path;
	return path;
}

function windowsPathEntries(environment: NodeJS.ProcessEnv): string[] {
	const raw = environmentValue(environment, "PATH", true) ?? "";
	return raw.split(";").map((entry) => {
		const trimmed = entry.trim();
		return trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')
			? trimmed.slice(1, -1)
			: trimmed;
	}).filter(Boolean);
}

function windowsPathExtensions(environment: NodeJS.ProcessEnv): string[] {
	const raw = environmentValue(environment, "PATHEXT", true) ?? ".COM;.EXE;.BAT;.CMD";
	return raw.split(";").filter(Boolean).map((extension) =>
		extension.startsWith(".") ? extension : `.${extension}`,
	);
}

function expandWindowsEnvironment(value: string, environment: NodeJS.ProcessEnv): string {
	return value.replace(/%([^%]+)%/g, (whole, name: string) =>
		environmentValue(environment, name, true) ?? whole,
	);
}

function executableFile(path: string): string | null {
	try {
		return statSync(path).isFile() ? path : null;
	} catch {
		return null;
	}
}

/** Resolve an executable using PATH/PATHEXT and filesystem APIs only. */
export function resolveWindowsExecutable(
	command: string,
	environment: NodeJS.ProcessEnv = process.env,
): string | null {
	const expanded = expandWindowsEnvironment(command, environment).trim();
	if (!expanded) return null;
	const hasPath = win32.isAbsolute(expanded) || expanded.includes("\\") || expanded.includes("/");
	const roots = hasPath ? [""] : windowsPathEntries(environment);
	const extensions = win32.extname(expanded) ? [""] : windowsPathExtensions(environment);
	for (const root of roots) {
		for (const extension of extensions) {
			const candidate = root ? win32.join(root, `${expanded}${extension}`) : `${expanded}${extension}`;
			const found = executableFile(candidate);
			if (found) return found;
		}
	}
	return null;
}

function resolveWindowsShell(environment: NodeJS.ProcessEnv): string {
	const configured = environmentValue(environment, "ROOST_SHELL", true);
	if (configured) {
		const resolved = resolveWindowsExecutable(configured, environment);
		if (!resolved) throw new Error(`ROOST_SHELL executable not found: ${configured}`);
		return resolved;
	}
	const pwsh = resolveWindowsExecutable("pwsh.exe", environment);
	if (pwsh) return pwsh;
	let powershell = resolveWindowsExecutable("powershell.exe", environment);
	const systemRoot = environmentValue(environment, "SystemRoot", true);
	if (!powershell && systemRoot) {
		powershell = executableFile(
			win32.join(systemRoot, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
		);
	}
	if (powershell) return powershell;
	const commandProcessor = environmentValue(environment, "COMSPEC", true);
	let cmd = resolveWindowsExecutable("cmd.exe", environment);
	if (!cmd && commandProcessor && win32.basename(commandProcessor).toLowerCase() === "cmd.exe") {
		cmd = resolveWindowsExecutable(commandProcessor, environment);
	}
	if (!cmd && systemRoot) cmd = executableFile(win32.join(systemRoot, "System32", "cmd.exe"));
	if (cmd) return cmd;
	throw new Error("no supported Windows shell found (pwsh.exe, powershell.exe, cmd.exe)");
}

function posixSpec(options: ResolveShellSpecOptions, platform: "darwin" | "linux"): ShellSpec {
	const environment = options.environment ?? process.env;
	const executable = environment.SHELL ?? "/bin/bash";
	if (executable.length === 0) throw new Error("SHELL must name an executable");
	const name = executable.replace(/\\/g, "/").split("/").at(-1)?.toLowerCase();
	const history = withHistfile(options.cwd);
	const common = {
		TERM: "xterm-256color",
		COLORTERM: "truecolor",
		LANG: environment.LANG || (platform === "darwin" ? "en_US.UTF-8" : "C.UTF-8"),
		LC_ALL: environment.LC_ALL || (platform === "darwin" ? "en_US.UTF-8" : "C.UTF-8"),
		PROMPT_EOL_MARK: "",
		...(platform === "darwin" ? { TERM_PROGRAM: "Apple_Terminal" } : {}),
	};
	let argv: string[] = [];
	let shellEnvironment: Record<string, string> = {};
	if (name === "bash") argv = ["--rcfile", ensurePosixBootstrap("bash")];
	if (name === "zsh") shellEnvironment = { ZDOTDIR: ensurePosixBootstrap("zsh") };
	return {
		version: 1,
		platform,
		executable,
		argv,
		cwd: options.cwd,
		env: mergeEnvironment(
			platform,
			environment,
			common,
			shellEnvironment,
			history,
			options.envOverlay ?? {},
			{ ROOST_SESSION_ID: options.sessionId },
		),
	};
}

function windowsSpec(options: ResolveShellSpecOptions): ShellSpec {
	const environment = options.environment ?? process.env;
	const cwd = normalizeNativePath("win32", options.cwd);
	const executable = resolveWindowsShell(environment);
	const name = win32.basename(executable).toLowerCase();
	const powerShell = name === "pwsh.exe" || name === "powershell.exe";
	const common: Record<string, string> = {
		TERM: "xterm-256color",
		COLORTERM: "truecolor",
	};
	if (powerShell) common.ROOST_PSREADLINE_HISTORY = psReadLineHistoryPath(cwd);
	return {
		version: 1,
		platform: "win32",
		executable,
		argv: powerShell ? ["-NoLogo", "-NoExit", "-Command", POWERSHELL_BOOTSTRAP] : [],
		cwd,
		env: mergeEnvironment(
			"win32",
			environment,
			common,
			withHistfile(cwd),
			options.envOverlay ?? {},
			{ ROOST_SESSION_ID: options.sessionId },
		),
	};
}

export function resolveShellSpec(options: ResolveShellSpecOptions): ShellSpec {
	const platform = options.platform ?? supportedHostPlatform();
	switch (platform) {
		case "darwin":
		case "linux":
			return posixSpec(options, platform);
		case "win32":
			return windowsSpec(options);
		default:
			return assertNeverPlatform(platform);
	}
}

export function isShellSpec(value: unknown): value is ShellSpec {
	if (!value || typeof value !== "object") return false;
	const spec = value as Partial<ShellSpec>;
	if (spec.version !== 1 || !isSupportedHostPlatform(spec.platform)) return false;
	if (typeof spec.executable !== "string" || spec.executable.length === 0) return false;
	if (typeof spec.cwd !== "string" || spec.cwd.length === 0) return false;
	if (!Array.isArray(spec.argv) || !spec.argv.every((arg) => typeof arg === "string")) return false;
	if (!spec.env || typeof spec.env !== "object" || Array.isArray(spec.env)) return false;
	return Object.entries(spec.env).every(([key, entry]) =>
		key.length > 0
		&& typeof entry === "string"
		&& !isKeeperControlEnvironmentKey(key)
	);
}
