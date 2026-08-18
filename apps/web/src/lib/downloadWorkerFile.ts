// Download a file that lives on a worker, straight to the browser. Loops the
// chunked filesReadChunk RPC (worker byte-range reads → coord relays → here) so
// the transfer card shows real progress/speed/ETA and there is no size cap. The
// bytes still assemble into one in-memory Blob for the <a download> (same as
// the old unary path; only the transport is chunked). Works whether the session
// is on this Mac or another on the tailnet. Wired to terminal file-link clicks
// (CellTerminal): Cmd/Ctrl-click a path in the terminal → the file downloads.

import { coordClient } from "../connect.ts";
import { addToast } from "../store/toastStore.ts";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { addTransfer, markTransferState, setTransferProgress } from "../store/transfers.ts";
import { log } from "@roost/shared/log";
import { parseWorkerFileHref, workerPathBasename } from "./nativePath.ts";

// 4 MiB per chunk — matches the upload chunk size; bounded memory per hop.
const DOWNLOAD_CHUNK = 4 * 1024 * 1024;

/** Parse the internal `/file/<workerFp>/<encoded native path>[#L<n>]` href. */
export const parseFileHref = parseWorkerFileHref;

/** Fetch a worker file's bytes in chunks and trigger a browser download. */
export async function downloadWorkerFileByHref(href: string): Promise<void> {
  const parsed = parseFileHref(href);
  if (!parsed) { addToast("Bad file link", "err"); return; }
  const sourceName = workerPathBasename(parsed.workerFp, parsed.path) || "download";
  const renderMarkdown = /\.md$/i.test(sourceName);
  const downloadName = renderMarkdown ? sourceName.replace(/\.md$/i, ".html") : sourceName;
  const id = crypto.randomUUID();
  addTransfer({ id, name: downloadName, dir: "down", bytes_total: 0, state: "active" });
  try {
    const parts: BlobPart[] = [];
    let offset = 0;
    let total = 0;
    for (;;) {
      const res = await coordClient.filesReadChunk({
        workerFp: parsed.workerFp, path: parsed.path, offset: BigInt(offset), len: DOWNLOAD_CHUNK,
      });
      total = Number(res.size);
      if (res.data.length) { parts.push(res.data as BlobPart); offset += res.data.length; }
      setTransferProgress(id, offset, total);
      // eof from the worker, or a defensive stop if a chunk returns nothing.
      if (res.eof || res.data.length === 0) break;
    }
    let blob = new Blob(parts);
    if (renderMarkdown) {
      const markdown = await blob.text();
      const body = DOMPurify.sanitize(await marked.parse(markdown));
      blob = new Blob([
        `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"></head><body>${body}</body></html>`,
      ], { type: "text/html;charset=utf-8" });
    }
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = downloadName;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    markTransferState(id, "ok");
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    log.warn("downloadWorkerFile", "download_failed", { msg });
    markTransferState(id, "err", msg);
    addToast(`Download failed: ${msg}`, "err");
  }
}
