// att1d — SPA file upload primitive. Drag/paste/pick a File →
// stream bytes to coord via chunked Connect AttachFileChunk → return abs_path
// that the SPA injects into the PTY via Sync v2.
//
// att1-stream: NO size ceiling. The file is sliced with Blob.slice into
// bounded chunks and sent one unary AttachFileChunk per slice (serial, in
// order). connect-web can't request-stream over fetch, so chunked-unary is
// the bounded-memory path. coord relays each chunk to the worker raw (no
// base64); the worker assembles to a temp file and returns abs_path on the
// final chunk. Memory is O(chunk) on every hop — a multi-GB file never sits
// in one buffer.
//
// Serial queues preserve direct-upload order and serialize each complete
// hash → probe → upload → sink operation selected through an attachment entry.

import { coordClient } from "../connect.ts";
import { log } from "@roost/shared/log";
import { sendUserTerminalInput } from "./userTerminalInput.ts";
import { addTransfer, markTransferState, setTransferProgress } from "../store/transfers.ts";
import type { Session } from "@roost/shared/wire";

// 4 MiB per chunk — well under any Connect message limit, still O(chunk)
// memory, but 4x fewer ordered round-trips (and 4x fewer coord session→worker
// lookups) than 1 MiB. Chunks are sent serially, so chunk size — not
// concurrency — is the lever for upload throughput on large files.
const CHUNK_BYTES = 4 * 1024 * 1024;

// Serial queue: each call awaits the prior chain link.
let uploadQueue: Promise<unknown> = Promise.resolve();
let attachmentQueue: Promise<unknown> = Promise.resolve();

export interface UploadResult {
  abs_path: string;
}

// Client-side dedup probe cap. Above this we skip the SHA-256 (hashing a
// multi-GB file in the browser would defeat the O(chunk) memory design) and
// upload directly. The worker still records the content hash on save.
const DEDUP_MAX_BYTES = 64 * 1024 * 1024;

async function sha256Hex(file: File): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", await file.arrayBuffer());
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function getShortPathPref(): boolean {
  try { return localStorage.getItem("roost.useShortAttachPaths") === "1"; }
  catch { return false; }
}

export function setShortPathPref(v: boolean): void {
  try { localStorage.setItem("roost.useShortAttachPaths", v ? "1" : "0"); }
  catch { /* private mode */ }
}

async function uploadAttachmentWithPref(
  session: { id: string },
  file: File,
  shortPath: boolean,
  onProgress?: (bytesSent: number) => void,
): Promise<UploadResult> {
  const uploadId = crypto.randomUUID();
  // Chain serially; preserve drop order even when caller awaits concurrently.
  const myTurn = uploadQueue.then(async () => {
    let absPath = "";
    let seq = 0;
    // Always send at least one (possibly empty) chunk so a 0-byte file still
    // creates the file and returns a path. `last` flags the final chunk; `seq`
    // lets the worker reject gaps/reorders (no truncated files on retry).
    for (let offset = 0; offset === 0 || offset < file.size; offset += CHUNK_BYTES) {
      const slice = file.slice(offset, offset + CHUNK_BYTES);
      const data = new Uint8Array(await slice.arrayBuffer());
      const last = offset + CHUNK_BYTES >= file.size;
      const res = await coordClient.attachFileChunk({
        uploadId,
        sessionId: session.id,
        filename: file.name,
        shortPath,
        data,
        last,
        seq: seq++,
      });
      if (last) absPath = res.absPath;
      onProgress?.(Math.min(offset + CHUNK_BYTES, file.size));
    }
    return { abs_path: absPath };
  });
  uploadQueue = myTurn.catch(() => undefined);  // don't poison chain
  return myTurn;
}

export async function uploadAttachment(
  session: { id: string },  // only the id string is needed (wire field, unbranded)
  file: File,
  onProgress?: (bytesSent: number) => void,  // fired after each chunk is acked
): Promise<UploadResult> {
  return uploadAttachmentWithPref(session, file, getShortPathPref(), onProgress);
}

/** Upload one file with a live transfer card (hashing → dedup-probe → upload),
 *  then hand the resulting absolute path to `sink`. The sink is a parameter
 *  because the chat composer builds an attachment chip from it while the
 *  terminal writes it to the PTY — same upload, two destinations. The `File`
 *  rides along so a sink can read name/mime/size without re-probing disk. */
export async function enqueueAttachmentTo(session: Session, file: File, sink: (absPath: string, file: File) => void): Promise<void> {
  const id = crypto.randomUUID();
  addTransfer({ id, name: file.name, dir: "up", bytes_total: file.size, state: "hashing" });
  const myTurn = attachmentQueue.then(async () => {
    const shortPath = getShortPathPref();
    try {
      // Content dedup: hash first, ask the worker if it already holds these exact
      // bytes. Hit → reuse the existing path, skip the upload. Probe is
      // best-effort — any failure falls through to a normal upload.
      if (file.size > 0 && file.size <= DEDUP_MAX_BYTES) {
        try {
          const sha256 = await sha256Hex(file);
          const probe = await coordClient.attachmentProbe({
            sessionId: session.id, sha256, size: BigInt(file.size), filename: file.name, shortPath,
          });
          if (probe.hit) {
            sink(probe.absPath, file);
            markTransferState(id, "dedup");
            return;
          }
        } catch { /* probe best-effort: fall through to a normal upload */ }
      }
      markTransferState(id, "active");
      const res = await uploadAttachmentWithPref(
        session,
        file,
        shortPath,
        (sent) => setTransferProgress(id, sent),
      );
      sink(res.abs_path, file);
      markTransferState(id, "ok");
    } catch (err) {
      log.warn("attachments", "transfer_failed", { msg: String(err) });
      markTransferState(id, "err", err instanceof Error ? err.message : String(err));
    }
  });
  attachmentQueue = myTurn.catch(() => undefined);
  return myTurn;
}

/** Type an uploaded file's absolute path into the session PTY (trailing space). */
export function injectPath(session: Session, absPath: string): void {
  sendUserTerminalInput(session.id, new TextEncoder().encode(`${absPath} `));
}

/** Upload + inject the abs_path into the PTY (trailing space). Shared by
 *  CellTerminal + the context menu. */
export async function enqueueAttachment(session: Session, file: File): Promise<void> {
  return enqueueAttachmentTo(session, file, (p) => injectPath(session, p));
}

/** Native-picker knobs. `capture: "environment"` hands off to the rear camera
 *  on iOS/Android instead of opening the file browser. */
export interface PickOptions {
  /** `accept` attribute; omitted = any file. */
  accept?: string;
  capture?: "environment" | "user";
  /** default true */
  multiple?: boolean;
}

/** Open the native file picker (must run inside a user gesture) and run each
 *  chosen file through `sink`. */
export function pickFilesTo(
  session: Session,
  sink: (absPath: string, file: File) => void,
  opts?: PickOptions,
): void {
  const input = document.createElement("input");
  input.type = "file";
  input.multiple = opts?.multiple ?? true;
  if (opts?.accept) input.accept = opts.accept;
  // setAttribute, not `input.capture =`: lib.dom declares the IDL property but
  // Chromium doesn't implement it, so the assignment becomes a dead expando and
  // never reaches the attribute the picker actually reads. The attribute path
  // is honored by every engine that supports HTML Media Capture.
  if (opts?.capture) input.setAttribute("capture", opts.capture);
  input.style.display = "none";
  let cleaned = false;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    input.remove();
  };
  input.onchange = () => {
    try {
      const files = input.files;
      if (files) for (let i = 0; i < files.length; i++) void enqueueAttachmentTo(session, files[i]!, sink);
    } finally {
      cleanup();
    }
  };
  input.oncancel = cleanup;
  document.body.appendChild(input);
  input.click();
}

/** Native picker → each chosen file uploaded and injected into the PTY. */
export function pickAndAttachFiles(session: Session, opts?: PickOptions): void {
  pickFilesTo(session, (p) => injectPath(session, p), opts);
}
