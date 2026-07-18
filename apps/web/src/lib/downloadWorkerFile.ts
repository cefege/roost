// Download a file that lives on a worker, straight to the browser. Loops the
// chunked filesReadChunk RPC (worker byte-range reads → coord relays → here) so
// the transfer card shows real progress/speed/ETA and there is no size cap. The
// bytes still assemble into one in-memory Blob for the <a download> (same as
// the old unary path; only the transport is chunked). Works whether the session
// is on this Mac or another on the tailnet. Wired to terminal file-link clicks
// (CellTerminal): Cmd/Ctrl-click a path in the terminal → the file downloads.

import { coordClient } from "../connect.ts";
import { addToast } from "./toastStore.ts";
import { addTransfer, markTransferState, setTransferProgress } from "../store/transfers.ts";
import { log } from "@roost/shared/log";

// 4 MiB per chunk — matches the upload chunk size; bounded memory per hop.
const DOWNLOAD_CHUNK = 4 * 1024 * 1024;

/** Parse the internal `/file/<workerFp>/<enc path>[#L<n>]` href produced by
 *  CellTerminal's resolveFile back into { workerFp, absolute path }. */
export function parseFileHref(href: string): { workerFp: string; path: string } | null {
  const noHash = href.replace(/#L\d+$/, "");
  const m = noHash.match(/^\/file\/([^/]+)\/(.*)$/);
  if (!m) return null;
  const path = "/" + m[2].split("/").map(decodeURIComponent).join("/");
  return { workerFp: m[1], path };
}

/** Fetch a worker file's bytes in chunks and trigger a browser download. */
export async function downloadWorkerFileByHref(href: string): Promise<void> {
  const parsed = parseFileHref(href);
  if (!parsed) { addToast("Bad file link", "err"); return; }
  const name = parsed.path.split("/").pop() || "download";
  const id = crypto.randomUUID();
  addTransfer({ id, name, dir: "down", bytes_total: 0, state: "active" });
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
    const url = URL.createObjectURL(new Blob(parts));
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
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
