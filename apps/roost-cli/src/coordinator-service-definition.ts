// Parses installed coordinator service definitions and builds lifecycle commands.
// The deploy journal validates persisted identity through these parsers; rollback
// uses the commands to stop the target fully before replacing SQLite and to
// restart the exact configured coordinator service.
import { coordServiceLabel } from "@roost/shared/paths";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import { launchdBootstrapWithRetryCmd } from "./service-ctl.ts";

function coordinatorServiceLabelForPlatform(platform: NodeJS.Platform): string {
  if (platform !== "darwin" && platform !== "linux") {
    throw new Error(`unsupported POSIX coordinator platform ${platform}`);
  }
  return coordServiceLabel(process.env, platform);
}

export function coordinatorRepoFromService(
  definition: string,
  platform: NodeJS.Platform,
): string | null {
  if (platform === "linux") {
    const match = /^WorkingDirectory=(?:"((?:\\.|[^"])*)"|([^\r\n]*))$/m.exec(definition);
    const value = match?.[1] ?? match?.[2];
    return value
      ? value.replace(/\\([\\\"nrt])/g, (_full, escaped: string) => {
        if (escaped === "n") return "\n";
        if (escaped === "r") return "\r";
        if (escaped === "t") return "\t";
        return escaped;
      }).trim() || null
      : null;
  }
  if (platform === "darwin") {
    const value = /<key>WorkingDirectory<\/key>\s*<string>([^<]+)<\/string>/.exec(definition)?.[1];
    return value
      ? value
        .replaceAll("&lt;", "<")
        .replaceAll("&gt;", ">")
        .replaceAll("&quot;", "\"")
        .replaceAll("&apos;", "'")
        .replaceAll("&amp;", "&")
        .trim() || null
      : null;
  }
  return null;
}

export function coordinatorInstallEnvironment(
  definition: string,
  platform: "darwin" | "linux",
): Record<string, string> {
  const environment = parsePosixServiceEnvironment(definition, platform);
  if (platform === "linux") {
    for (const [directive, key] of [
      ["MemoryHigh", "ROOST_COORD_MEMORY_HIGH"],
      ["MemoryMax", "ROOST_COORD_MEMORY_MAX"],
      ["TasksMax", "ROOST_COORD_TASKS_MAX"],
    ] as const) {
      const value = parseSystemdServiceDirective(definition, directive);
      if (value) environment[key] = value;
    }
  }
  if (environment.ROOST_FRONTED === undefined) {
    const bind = environment.ROOST_COORDINATOR_BIND;
    if (environment.ROOST_TRUST_PROXY === "1" || bind?.startsWith("127.0.0.1:")) {
      environment.ROOST_FRONTED = "1";
    } else if (bind || environment.ROOST_TLS_CERT_PATH || environment.ROOST_TLS_KEY_PATH) {
      environment.ROOST_FRONTED = "0";
    }
  }
  return environment;
}

export function coordinatorStopCommand(
  platform: NodeJS.Platform = process.platform,
  label: string = coordinatorServiceLabelForPlatform(platform),
): string {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    return `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; `
      + `systemctl --user stop ${posixShellQuote(unit)}`;
  }
  if (platform !== "darwin") throw new Error(`unsupported POSIX coordinator platform ${platform}`);
  const job = `gui/$uid/${posixShellQuote(label)}`;
  return `set -e; uid=$(id -u); launchctl bootout ${job} 2>/dev/null || true; `
    + `for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do `
    + `if ! launchctl print ${job} >/dev/null 2>&1; then exit 0; fi; `
    + `sleep 0.25; done; echo 'coordinator bootout did not settle' >&2; exit 1`;
}

export function coordinatorRestartCommand(
  servicePath: string,
  platform: NodeJS.Platform = process.platform,
  label: string = coordinatorServiceLabelForPlatform(platform),
): string {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    return `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; `
      + `systemctl --user daemon-reload && systemctl --user restart ${posixShellQuote(unit)}`;
  }
  if (platform !== "darwin") throw new Error(`unsupported POSIX coordinator platform ${platform}`);
  return launchdBootstrapWithRetryCmd(label, servicePath, { role: "coordinator rollback" });
}
