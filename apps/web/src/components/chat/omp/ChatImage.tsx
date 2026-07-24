// ChatImage — renders an omp image block. `data:` URLs (inline base64) paint
// directly. Worker-fs blob paths (blob:sha256 → ~/.omp/agent/blobs/<hash>) are
// fetched as bytes via the chunked filesReadChunk RPC and inlined as a data URL
// (CSP allows img-src data:; blob:/object URLs are not permitted for <img>).
// Failure or no worker → a visible labeled placeholder, never a silent drop.

import { createResource, Show } from "solid-js";
import { coordClient } from "../../../connect.ts";
import { rootStore } from "../../../store/root.ts";

interface Props {
  sessionId: string;
  blobPath: string;
  mime: string;
}

function bytesToDataUrl(bytes: Uint8Array, mime: string): string {
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return `data:${mime};base64,${btoa(bin)}`;
}

async function fetchBlob(sessionId: string, path: string, mime: string): Promise<string | null> {
  const workerFp = rootStore.sessions[sessionId]?.worker_fp;
  if (!workerFp) return null;
  const chunks: Uint8Array[] = [];
  let offset = 0;
  for (;;) {
    const res = await coordClient.filesReadChunk({ workerFp, path, offset: BigInt(offset), len: 4 * 1024 * 1024 });
    if (res.data.length) { chunks.push(res.data); offset += res.data.length; }
    if (res.eof || res.data.length === 0) break;
  }
  if (chunks.length === 0) return null;
  const total = chunks.reduce((n, c) => n + c.length, 0);
  const bytes = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) { bytes.set(c, at); at += c.length; }
  return bytesToDataUrl(bytes, mime);
}

export function ChatImage(props: Props) {
  const isData = () => props.blobPath.startsWith("data:");
  const [src] = createResource(
    () => (isData() ? null : props.blobPath),
    (path) => fetchBlob(props.sessionId, path, props.mime),
  );
  const url = () => (isData() ? props.blobPath : src());
  return (
    <Show when={url()} fallback={<span class="omp-img-placeholder">🖼 image ({props.mime}){src.loading ? " loading…" : ""}</span>}>
      <img class="omp-img" src={url()!} alt="" loading="lazy" />
    </Show>
  );
}
