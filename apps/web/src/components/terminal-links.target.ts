// Validates terminal-authored URLs and file targets before any anchor opens.
// Link detection and the DOM linkifier share this authority so inferred and
// producer-painted links receive the same protocol and worker-route checks.
// Worker-aware callers supply the only path-to-route resolver.

import { linkUriWithinCap } from "@roost/shared/cell";

/** Resolve a raw file path (+ optional 1-based line) from terminal output into
 * an internal `/file/<workerFp>/…#L<line>` href, or null to skip linkifying it.
 * `fileAuthority` is present only for a `file://host/path` target. */
export type ResolveFile = (
  rawPath: string,
  line: number | null,
  fileAuthority?: string,
) => string | null;

/** Split a trailing `:line[:col]` off a file candidate. The viewer currently
 * has a line contract, not a column contract, so a valid column is consumed. */
function splitPathLine(raw: string): { path: string; line: number | null } {
  const match = raw.match(/^(.*?):(\d+)(?::\d+)?$/);
  if (!match) return { path: raw, line: null };
  const line = Number(match[2]);
  return Number.isSafeInteger(line) && line > 0
    ? { path: match[1], line }
    : { path: raw, line: null };
}

export interface ExternalTerminalLinkTarget {
  kind: "external";
  href: string;
  display: string;
}

export interface WorkerFileTerminalLinkTarget {
  kind: "file";
  rawPath: string;
  line: number | null;
  fileAuthority?: string;
  /** Null until a worker-aware resolver is available. */
  href: string | null;
  display: string;
}

export type TerminalLinkTarget =
  | ExternalTerminalLinkTarget
  | WorkerFileTerminalLinkTarget;

const URI_SCHEME_RE = /^[A-Za-z][A-Za-z0-9+.-]*:/;
export const WINDOWS_DRIVE_ABS_RE = /^[A-Za-z]:[\\/]/;
const WINDOWS_UNC_RE = /^\\\\[^\\]+\\[^\\]+/;
const ASCII_SPACE_OR_CONTROL_RE = /[\u0000-\u0020\u007f]/;
export const FILE_NAME_RE = /^[^/\\:]+\.[A-Za-z][\w-]{0,15}(?::\d+(?::\d+)?)?$/;

/** Only routes produced by workerFileHref may be installed on an internal
 * terminal anchor. Query strings and arbitrary fragments are never accepted. */
export function isWorkerFileHref(href: string): boolean {
  return /^\/file\/[^/?#]+\/[^?#]+(?:#L[1-9]\d*)?$/.test(href)
    && linkUriWithinCap(href);
}

function resolvedFileTarget(
  rawPath: string,
  line: number | null,
  display: string,
  resolveFile: ResolveFile | undefined,
  fileAuthority?: string,
): WorkerFileTerminalLinkTarget | null {
  if (!rawPath || /[\u0000-\u001f\u007f]/.test(rawPath)) return null;
  if (!resolveFile) {
    return { kind: "file", rawPath, line, fileAuthority, href: null, display };
  }
  let href: string | null;
  try {
    href = resolveFile(rawPath, line, fileAuthority);
  } catch {
    return null;
  }
  if (!href || !isWorkerFileHref(href)) return null;
  return { kind: "file", rawPath, line, fileAuthority, href, display };
}

function classifyFileUri(
  rawTarget: string,
  resolveFile: ResolveFile | undefined,
): WorkerFileTerminalLinkTarget | null {
  // Requiring `//` rejects browser-style `file:relative` coercion.
  if (!/^file:\/\//i.test(rawTarget)) return null;
  try {
    const url = new URL(rawTarget);
    if (url.protocol !== "file:" || url.username || url.password || url.search) return null;
    let line: number | null = null;
    if (url.hash) {
      const match = url.hash.match(/^#L([1-9]\d*)$/);
      if (!match) return null;
      line = Number(match[1]);
      if (!Number.isSafeInteger(line)) return null;
    }
    let rawPath = decodeURIComponent(url.pathname);
    // WHATWG file URLs spell a Windows drive as /C:/path.
    if (/^\/[A-Za-z]:[\\/]/.test(rawPath)) rawPath = rawPath.slice(1);
    const split = line === null ? splitPathLine(rawPath) : { path: rawPath, line };
    const authority = url.hostname && url.hostname.toLowerCase() !== "localhost"
      ? decodeURIComponent(url.hostname)
      : undefined;
    return resolvedFileTarget(
      split.path,
      split.line,
      rawTarget,
      resolveFile,
      authority,
    );
  } catch {
    return null;
  }
}

/** Classify untrusted terminal output before an anchor is painted or activated.
 * External navigation is deliberately limited to absolute HTTP(S), while file
 * targets must resolve to an authenticated worker route before activation. */
export function classifyTerminalLinkTarget(
  rawTarget: string,
  resolveFile?: ResolveFile,
): TerminalLinkTarget | null {
  if (
    !rawTarget
    || rawTarget !== rawTarget.trim()
    || ASCII_SPACE_OR_CONTROL_RE.test(rawTarget)
    || !linkUriWithinCap(rawTarget)
  ) return null;

  if (/^https?:\/\//i.test(rawTarget)) {
    try {
      const url = new URL(rawTarget);
      if (
        (url.protocol !== "https:" && url.protocol !== "http:")
        || !url.hostname
      ) return null;
      return { kind: "external", href: rawTarget, display: rawTarget };
    } catch {
      return null;
    }
  }

  if (/^file:/i.test(rawTarget)) return classifyFileUri(rawTarget, resolveFile);

  // `//host/share` is ambiguous with a protocol-relative URL. It can only
  // survive when the worker-aware resolver accepts it as a Windows UNC path.
  if (rawTarget.startsWith("//")) {
    if (!resolveFile) return null;
    const { path, line } = splitPathLine(rawTarget);
    return resolvedFileTarget(path, line, rawTarget, resolveFile);
  }
  const windowsPath = WINDOWS_DRIVE_ABS_RE.test(rawTarget)
    || WINDOWS_UNC_RE.test(rawTarget);
  const explicitFileName = FILE_NAME_RE.test(rawTarget);
  if (!windowsPath && !explicitFileName && URI_SCHEME_RE.test(rawTarget)) return null;
  const pathLike = windowsPath
    || rawTarget.startsWith("/")
    || rawTarget.startsWith("./")
    || rawTarget.startsWith("../")
    || rawTarget.startsWith("~/")
    || rawTarget.includes("/")
    || rawTarget.includes("\\")
    || explicitFileName;
  if (!pathLike) return null;
  const { path, line } = splitPathLine(rawTarget);
  return resolvedFileTarget(path, line, rawTarget, resolveFile);
}
