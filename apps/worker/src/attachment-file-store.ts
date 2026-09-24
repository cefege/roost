// Attachment destination storage: final naming, short paths, and content manifest.
// AttachmentOperationOwner calls this only after its durable receipt state is ready;
// browser commands reuse the manifest probe without owning upload lifecycle.

import fs from "node:fs";
import path from "node:path";
import { log } from "@roost/observability/log";
import { assertNeverPlatform, supportedHostPlatform } from "@roost/platform/platform";
import { MANIFEST_NAME, resolveSessionDirWithinBase, sanitizeAttachmentName } from "./attachment-reaper.ts";
import { normalizeWorkerPath } from "./util/path.ts";
import { sha256AttachmentFile } from "./attachment-file-hash.ts";

const HOST_PLATFORM = supportedHostPlatform();

export interface AttachmentDestination {
  readonly fileName: string;
  readonly filePath: string;
}

export interface CommittedAttachmentDestination {
  readonly filePath: string;
  readonly verifiedExistingFile: boolean;
}

export function reserveAttachmentDestination(dir: string, filename: string): AttachmentDestination {
  const fileName = uniqueName(dir, sanitizeAttachmentName(filename));
  return { fileName, filePath: path.join(dir, fileName) };
}

/** Occupies the reserved name without yielding; a foreign occupant with other bytes is refused. */
export function placeAttachmentDestination(
  dir: string,
  tmpPath: string,
  fileName: string,
  sha256: string,
): CommittedAttachmentDestination {
  const filePath = path.join(dir, fileName);
  const finalAlreadyExists = fs.existsSync(filePath);
  if (finalAlreadyExists && sha256AttachmentFile(filePath) !== sha256) {
    throw new Error("attachment destination hash mismatch");
  }
  if (!finalAlreadyExists) fs.renameSync(tmpPath, filePath);
  return { filePath, verifiedExistingFile: finalAlreadyExists };
}

/** Synchronous commit for crash recovery; live uploads flush asynchronously instead. */
export function commitAttachmentDestination(
  dir: string,
  tmpPath: string,
  fileName: string,
  sha256: string,
): CommittedAttachmentDestination {
  const placed = placeAttachmentDestination(dir, tmpPath, fileName, sha256);
  const fd = fs.openSync(placed.filePath, "r");
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
  syncAttachmentDirectory(dir);
  recordAttachmentHash(dir, sha256, fileName);
  return placed;
}

export async function syncAttachmentFileAsync(filePath: string): Promise<void> {
  const file = await fs.promises.open(filePath, "r");
  try { await file.sync(); } finally { await file.close(); }
}

/** Flushes a rename's directory entry; POSIX failures propagate so no receipt outruns disk. */
export function syncAttachmentDirectory(directory: string): void {
  switch (HOST_PLATFORM) {
    case "darwin":
    case "linux": {
      const fd = fs.openSync(directory, "r");
      try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
      return;
    }
    case "win32":
      // Windows cannot open a directory for fsync; NTFS journals the rename itself.
      return;
    default:
      return assertNeverPlatform(HOST_PLATFORM);
  }
}

export async function syncAttachmentDirectoryAsync(directory: string): Promise<void> {
  switch (HOST_PLATFORM) {
    case "darwin":
    case "linux": {
      const handle = await fs.promises.open(directory, "r");
      try { await handle.sync(); } finally { await handle.close(); }
      return;
    }
    case "win32":
      return;
    default:
      return assertNeverPlatform(HOST_PLATFORM);
  }
}

export function attachmentReplyPath(dir: string, filePath: string, shortPath: boolean): string {
  const replyPath = shortPath ? shortAttachmentPath(dir, filePath) : filePath;
  return normalizeWorkerPath(replyPath, HOST_PLATFORM);
}

export function recordAttachmentHash(dir: string, sha256: string, fname: string): void {
  const manifest = loadManifest(dir);
  for (const key of Object.keys(manifest)) {
    if (manifest[key] === fname) delete manifest[key];
  }
  manifest[sha256] = fname;
  saveManifest(dir, manifest);
}

export function probeAttachment(
  sessionId: string,
  sha256: string,
  shortPath: boolean,
): { hit: boolean; abs_path: string } {
  const dir = resolveSessionDirWithinBase(sessionId);
  if (!dir) return { hit: false, abs_path: "" };
  const fname = loadManifest(dir)[sha256];
  if (!fname) return { hit: false, abs_path: "" };
  const filePath = path.join(dir, fname);
  if (!fs.existsSync(filePath)) return { hit: false, abs_path: "" };
  const replyPath = shortPath ? shortAttachmentPath(dir, filePath) : filePath;
  return { hit: true, abs_path: normalizeWorkerPath(replyPath, HOST_PLATFORM) };
}

function uniqueName(dir: string, sanitized: string): string {
  if (!fs.existsSync(path.join(dir, sanitized))) return sanitized;
  const extension = path.extname(sanitized);
  const stem = sanitized.slice(0, sanitized.length - extension.length);
  for (let index = 2; index < 10_000; index += 1) {
    const candidate = `${stem} (${index})${extension}`;
    if (!fs.existsSync(path.join(dir, candidate))) return candidate;
  }
  return `${stem} (${Date.now()})${extension}`;
}

function shortAttachmentPath(dir: string, filePath: string): string {
  const shortcutsDir = path.join(dir, ".shortcuts");
  try {
    fs.mkdirSync(shortcutsDir, { recursive: true, mode: 0o700 });
    const occupied = new Set(fs.readdirSync(shortcutsDir).filter((name) => /^p\d+$/.test(name)));
    let index = 1;
    while (occupied.has(`p${index}`)) index += 1;
    const shortcutPath = path.join(shortcutsDir, `p${index}`);
    switch (HOST_PLATFORM) {
      case "darwin":
      case "linux":
        fs.symlinkSync(filePath, shortcutPath);
        break;
      case "win32":
        fs.copyFileSync(filePath, shortcutPath, fs.constants.COPYFILE_EXCL);
        break;
      default:
        return assertNeverPlatform(HOST_PLATFORM);
    }
    return shortcutPath;
  } catch (error) {
    log.warn("worker", "short_attachment_path_failed", {
      platform: HOST_PLATFORM,
      error: String(error),
    });
    return filePath;
  }
}

function loadManifest(dir: string): Record<string, string> {
  try {
    return JSON.parse(fs.readFileSync(path.join(dir, MANIFEST_NAME), "utf8")) as Record<string, string>;
  } catch {
    return {};
  }
}

function saveManifest(dir: string, manifest: Record<string, string>): void {
  try {
    fs.writeFileSync(path.join(dir, MANIFEST_NAME), JSON.stringify(manifest), { mode: 0o600 });
  } catch (error) {
    log.warn("worker", "manifest_write_failed", { error: String(error) });
  }
}

