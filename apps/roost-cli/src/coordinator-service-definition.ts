// Parses installed coordinator service definitions, applies selected endpoint
// settings, and builds lifecycle commands. The deploy journal validates persisted
// identity through these parsers; rollback uses the commands to stop the target
// fully before replacing SQLite and restart the exact configured service.
import { coordServiceLabel } from "@roost/host/paths";
import { posixShellQuote } from "@roost/platform/shell-quote";
import type { QuickstartEndpoint } from "./quickstart-endpoint.ts";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import { launchdBootstrapWithRetryCmd } from "./service-ctl.ts";

const ENDPOINT_ENVIRONMENT_KEYS = [
  "ROOST_TRUST_PROXY",
  "ROOST_WEB_PUBLIC_URL",
  "ROOST_CORS_ALLOWED_ORIGINS",
] as const;

type EndpointEnvironmentKey = typeof ENDPOINT_ENVIRONMENT_KEYS[number];
type EndpointEnvironmentValues = Record<EndpointEnvironmentKey, string>;

interface LaunchdEnvironmentRegion {
  bodyStart: number;
  bodyEnd: number;
}

interface SystemdServiceRegion {
  bodyStart: number;
  bodyEnd: number;
  headerEndsWithLineBreak: boolean;
}

interface SystemdEndpointEntry {
  key: EndpointEnvironmentKey;
  start: number;
  end: number;
  lineEnding: string;
}

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
  return environment;
}

/** Applies only the endpoint-owned environment entries to an installed service. */
export function coordinatorServiceWithEndpoint(
  definition: string,
  platform: "darwin" | "linux",
  endpoint: QuickstartEndpoint,
): string {
  const values: EndpointEnvironmentValues = {
    ROOST_TRUST_PROXY: endpoint.mode === "front-door" ? "1" : "0",
    ROOST_WEB_PUBLIC_URL: endpoint.webPublicUrl ?? "",
    ROOST_CORS_ALLOWED_ORIGINS: endpoint.corsAllowedOrigins.join(","),
  };
  return platform === "darwin"
    ? rewriteLaunchdEndpointEnvironment(definition, values)
    : rewriteSystemdEndpointEnvironment(definition, values);
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

function xmlEscape(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;")
    .replaceAll("'", "&apos;");
}

function systemdEnvironmentLine(key: EndpointEnvironmentKey, value: string): string {
  const escaped = value
    .replaceAll("%", "%%")
    .replaceAll("\\", "\\\\")
    .replaceAll("\"", "\\\"")
    .replaceAll("\n", "\\n")
    .replaceAll("\r", "\\r")
    .replaceAll("\t", "\\t");
  return `Environment="${key}=${escaped}"`;
}

function preferredLineEnding(value: string): string {
  return value.includes("\r\n") ? "\r\n" : "\n";
}

function findLaunchdEnvironmentRegion(definition: string): LaunchdEnvironmentRegion {
  const headers = [...definition.matchAll(/<key>\s*EnvironmentVariables\s*<\/key>\s*<dict>/g)];
  const header = headers[0];
  if (headers.length !== 1 || !header || header.index === undefined) {
    throw new Error("coordinator launchd service definition has ambiguous EnvironmentVariables");
  }
  const bodyStart = header.index + header[0].length;
  const bodyEnd = definition.indexOf("</dict>", bodyStart);
  const nestedDictionary = definition.indexOf("<dict>", bodyStart);
  if (bodyEnd === -1 || (nestedDictionary !== -1 && nestedDictionary < bodyEnd)) {
    throw new Error("coordinator launchd EnvironmentVariables is malformed");
  }
  return { bodyStart, bodyEnd };
}

function rewriteLaunchdEndpointEnvironment(
  definition: string,
  values: EndpointEnvironmentValues,
): string {
  const region = findLaunchdEnvironmentRegion(definition);
  let body = definition.slice(region.bodyStart, region.bodyEnd);
  const missing: EndpointEnvironmentKey[] = [];
  for (const key of ENDPOINT_ENVIRONMENT_KEYS) {
    const keyPattern = new RegExp(`<key>\\s*${key}\\s*</key>`, "g");
    const keys = [...body.matchAll(keyPattern)];
    if (keys.length === 0) {
      missing.push(key);
      continue;
    }
    const entryPattern = new RegExp(
      `(<key>\\s*${key}\\s*</key>)(\\s*)<string>([^<]*)</string>`,
      "g",
    );
    const entries = [...body.matchAll(entryPattern)];
    if (keys.length !== 1 || entries.length !== 1) {
      throw new Error(`coordinator launchd EnvironmentVariables has ambiguous ${key}`);
    }
    body = body.replace(
      entryPattern,
      (_matchedEntry, keyMarkup: string, spacing: string) =>
        `${keyMarkup}${spacing}<string>${xmlEscape(values[key])}</string>`,
    );
  }
  body = insertLaunchdEnvironmentEntries(body, missing, values);
  return definition.slice(0, region.bodyStart) + body + definition.slice(region.bodyEnd);
}

function insertLaunchdEnvironmentEntries(
  body: string,
  missing: readonly EndpointEnvironmentKey[],
  values: EndpointEnvironmentValues,
): string {
  if (missing.length === 0) return body;
  const lineEnding = preferredLineEnding(body);
  const closingIndentation = /(\r?\n)([ \t]*)$/.exec(body);
  const bodyBeforeClosingIndentation = closingIndentation
    ? body.slice(0, -closingIndentation[0].length)
    : body;
  let keyIndentation = "";
  const keyIndentationPattern = /(?:^|\r?\n)([ \t]+)<key>[^<]+<\/key>/g;
  let keyMatch: RegExpExecArray | null;
  while ((keyMatch = keyIndentationPattern.exec(body)) !== null) {
    keyIndentation = keyMatch[1]!;
  }
  if (!keyIndentation) keyIndentation = `${closingIndentation?.[2] ?? ""}  `;
  const entries = missing.map((key) =>
    `${keyIndentation}<key>${key}</key>${lineEnding}`
      + `${keyIndentation}<string>${xmlEscape(values[key])}</string>`
  ).join(lineEnding);
  const separator = bodyBeforeClosingIndentation.length === 0
    || bodyBeforeClosingIndentation.endsWith("\n")
    ? ""
    : lineEnding;
  const closing = closingIndentation?.[0] ?? lineEnding;
  return `${bodyBeforeClosingIndentation}${separator}${entries}${closing}`;
}

function findSystemdServiceRegion(definition: string): SystemdServiceRegion {
  const headers = [...definition.matchAll(/^\[Service\][ \t]*(?:\r?\n|$)/gm)];
  const header = headers[0];
  if (headers.length !== 1 || !header || header.index === undefined) {
    throw new Error("coordinator systemd service definition has ambiguous [Service] section");
  }
  const bodyStart = header.index + header[0].length;
  const nextSectionPattern = /^\[[^\]\r\n]+\][ \t]*(?:\r?\n|$)/gm;
  nextSectionPattern.lastIndex = bodyStart;
  const nextSection = nextSectionPattern.exec(definition);
  return {
    bodyStart,
    bodyEnd: nextSection?.index ?? definition.length,
    headerEndsWithLineBreak: header[0].endsWith("\n"),
  };
}

function rewriteSystemdEndpointEnvironment(
  definition: string,
  values: EndpointEnvironmentValues,
): string {
  const region = findSystemdServiceRegion(definition);
  const body = definition.slice(region.bodyStart, region.bodyEnd);
  const rewrittenBody = rewriteSystemdEnvironmentEntries(body, values);
  const headerLineBreak = !region.headerEndsWithLineBreak && rewrittenBody.length > 0
    ? preferredLineEnding(definition)
    : "";
  return definition.slice(0, region.bodyStart)
    + headerLineBreak
    + rewrittenBody
    + definition.slice(region.bodyEnd);
}

function rewriteSystemdEnvironmentEntries(
  body: string,
  values: EndpointEnvironmentValues,
): string {
  const entries: SystemdEndpointEntry[] = [];
  const environmentPattern = /^Environment=([^\r\n]*)(?:\r?\n|$)/gm;
  let environmentMatch: RegExpExecArray | null;
  while ((environmentMatch = environmentPattern.exec(body)) !== null) {
    const value = environmentMatch[1]!;
    for (const key of ENDPOINT_ENVIRONMENT_KEYS) {
      if (isSystemdEndpointEntry(value, key)) {
        if (entries.some((entry) => entry.key === key)) {
          throw new Error(`coordinator systemd Environment has duplicate ${key}`);
        }
        const matchedLine = environmentMatch[0]!;
        entries.push({
          key,
          start: environmentMatch.index,
          end: environmentMatch.index + matchedLine.length,
          lineEnding: matchedLine.endsWith("\r\n") ? "\r\n" : matchedLine.endsWith("\n") ? "\n" : "",
        });
      } else if (new RegExp(`(?:^|[\\s"])${key}=`).test(value)) {
        throw new Error(`coordinator systemd Environment has ambiguous ${key}`);
      }
    }
  }

  let rewritten = "";
  let previousEnd = 0;
  for (const entry of entries) {
    rewritten += body.slice(previousEnd, entry.start);
    rewritten += systemdEnvironmentLine(entry.key, values[entry.key]) + entry.lineEnding;
    previousEnd = entry.end;
  }
  rewritten += body.slice(previousEnd);

  const missing = ENDPOINT_ENVIRONMENT_KEYS.filter(
    (key) => !entries.some((entry) => entry.key === key),
  );
  return insertSystemdEnvironmentEntries(rewritten, missing, values);
}

function isSystemdEndpointEntry(value: string, key: EndpointEnvironmentKey): boolean {
  const quoted = new RegExp(`^"${key}=((?:\\\\.|[^"])*)"$`);
  if (quoted.test(value)) return true;
  const legacy = new RegExp(`^${key}=(.*)$`).exec(value);
  if (!legacy) return false;
  if (/(?:^|\s)[A-Za-z_][A-Za-z0-9_]*=/.test(legacy[1]!)) {
    throw new Error(`coordinator systemd Environment has ambiguous ${key}`);
  }
  return true;
}

function insertSystemdEnvironmentEntries(
  body: string,
  missing: readonly EndpointEnvironmentKey[],
  values: EndpointEnvironmentValues,
): string {
  if (missing.length === 0) return body;
  const lineEnding = preferredLineEnding(body);
  const environmentPattern = /^Environment=[^\r\n]*(?:\r?\n|$)/gm;
  let insertionPoint = 0;
  let environmentMatch: RegExpExecArray | null;
  while ((environmentMatch = environmentPattern.exec(body)) !== null) {
    insertionPoint = environmentMatch.index + environmentMatch[0]!.length;
  }
  const before = body.slice(0, insertionPoint);
  const after = body.slice(insertionPoint);
  const entries = missing.map((key) => systemdEnvironmentLine(key, values[key])).join(lineEnding);
  const beforeSeparator = before.length > 0 && !before.endsWith("\n") ? lineEnding : "";
  const afterSeparator = after.length > 0 && !after.startsWith("\n") && !after.startsWith("\r")
    ? lineEnding
    : "";
  return `${before}${beforeSeparator}${entries}${afterSeparator}${after}`;
}
