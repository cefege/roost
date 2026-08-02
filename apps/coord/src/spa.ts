import { existsSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

// Text assets worth compressing on the fly. woff2/wasm/png/jpg are already
// compressed — re-encoding them wastes CPU for ~0 gain, so they stream raw.
const COMPRESSIBLE_EXT: Record<string, true> = {
  ".js": true, ".css": true, ".html": true, ".json": true, ".svg": true, ".map": true, ".txt": true,
};

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml", ".png": "image/png",
  ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
  ".webp": "image/webp", ".avif": "image/avif",
  ".ico": "image/x-icon", ".wasm": "application/wasm",
  ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf",
  ".map": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".webmanifest": "application/manifest+json",
};

// Memoized gzip bodies, keyed by servable path and invalidated on an mtime/size
// mismatch so a rebuilt or re-embedded asset is never served stale. Bounded to
// 32 entries (oldest insertion dropped) — the SPA has ~10 compressible assets,
// so the cap only exists so a hostile URL space can't grow it.
const GZIP_CACHE_MAX = 32;
interface GzipEntry { mtimeMs: number; size: number; gz: Uint8Array<ArrayBuffer> }
const _gzipCache = new Map<string, GzipEntry>();

async function gzipCached(path: string): Promise<Uint8Array<ArrayBuffer>> {
  const file = Bun.file(path);
  const stat = await file.stat();
  const hit = _gzipCache.get(path);
  if (hit && hit.mtimeMs === stat.mtimeMs && hit.size === stat.size) return hit.gz;
  const gz = Bun.gzipSync(new Uint8Array(await file.arrayBuffer()));
  if (_gzipCache.size >= GZIP_CACHE_MAX) {
    const oldest = _gzipCache.keys().next();
    if (!oldest.done) _gzipCache.delete(oldest.value);
  }
  _gzipCache.set(path, { mtimeMs: stat.mtimeMs, size: stat.size, gz });
  return gz;
}

/**
 * Create a responder from exactly one complete SPA build. A valid on-disk
 * index wins for source runs; otherwise compiled installations use the
 * embedded manifest. Choosing once prevents mixing build generations.
 */
export function createSpaResponder(
  webDistPath: string | undefined,
  embeddedAssets: ReadonlyMap<string, string>,
): (url: URL, method: string, acceptEncoding: string) => Promise<Response> {
  const spaRoot = diskSpaRoot(webDistPath);
  const webAssets = spaRoot || embeddedAssets.size === 0 ? null : embeddedAssets;

  // rel (no leading slash) → servable path: an embedded-file path or a disk path.
  function resolveAsset(rel: string): string | null {
    if (!rel) return null;
    if (webAssets) return webAssets.get(rel) ?? null;
    if (!spaRoot) return null;
    const candidate = join(spaRoot, rel);
    const safe = candidate === spaRoot || candidate.startsWith(spaRoot + "/");
    return safe && existsSync(candidate) && statSync(candidate).isFile() ? candidate : null;
  }

  function resolveIndex(): string | null {
    if (webAssets) return webAssets.get("index.html") ?? null;
    return spaRoot ? join(spaRoot, "index.html") : null;
  }

  async function fileResponse(
    path: string, rel: string, ext: string, method: string, acceptEncoding: string, isIndex: boolean, hashed: boolean,
  ): Promise<Response> {
    const headers: Record<string, string> = {
      "content-type": MIME[ext] ?? "application/octet-stream",
    };
    if (isIndex) {
      headers["cache-control"] = "no-cache, no-store, must-revalidate";
    } else if (hashed) {
      // Vite content-hashed bundle (assets/*): the filename changes on every
      // content change, so the body at this URL is genuinely immutable.
      headers["cache-control"] = "public, max-age=31536000, immutable";
    } else if (rel.startsWith("fonts/")) {
      // The 4 woff2 faces are ~2.6 MB of stable-name assets, so `no-cache`
      // forced a conditional request for each on every cold load. A week is
      // long enough to stop that; deliberately NOT immutable, so swapping a
      // font still lands.
      headers["cache-control"] = "public, max-age=604800";
    } else {
      // Stable-filename assets (icons, manifest, wasm, whatsnew.json): the URL
      // is reused across builds, so it must NEVER be immutable — otherwise a
      // changed favicon/manifest stays pinned in the browser for a year.
      // Revalidate on every load instead.
      headers["cache-control"] = "no-cache";
    }
    // On-the-fly gzip via Bun's NATIVE Bun.gzipSync — NOT node:zlib (heap
    // corruption + random-later segfault under load; see git history +
    // feedback_no_connect_node_compression_under_bun). gzip only: Bun has no
    // native brotli sync, and gzip (~4.3x on the SPA chunks) is plenty.
    // Memoized: the 345 KB index, 625 KB vendor chunk and 72 KB CSS were
    // recompressed for every cold load by every client.
    if (COMPRESSIBLE_EXT[ext] && acceptEncoding.includes("gzip")) {
      const gz = await gzipCached(path);
      headers["content-encoding"] = "gzip";
      headers["vary"] = "accept-encoding";
      return new Response(method === "HEAD" ? null : gz, { status: 200, headers });
    }
    return new Response(Bun.file(path), { status: 200, headers });
  }

  return async function spaResponse(url: URL, method: string, acceptEncoding: string): Promise<Response> {
    if (method !== "GET" && method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const rel = url.pathname.replace(/^\/+/, "");
    const asset = resolveAsset(rel);
    if (asset) {
      const dot = rel.lastIndexOf(".");
      return fileResponse(asset, rel, dot >= 0 ? rel.slice(dot) : "", method, acceptEncoding, false, rel.startsWith("assets/"));
    }
    if (rel.startsWith("assets/")) {
      return new Response("not found", { status: 404 });
    }
    const index = resolveIndex();
    if (index) return fileResponse(index, "index.html", ".html", method, acceptEncoding, true, false);
    return new Response("not found", { status: 404 });
  };
}

function diskSpaRoot(webDistPath: string | undefined): string | null {
  if (!webDistPath) return null;
  const spaRoot = resolve(webDistPath);
  const indexPath = join(spaRoot, "index.html");
  return existsSync(indexPath) && statSync(indexPath).isFile() ? spaRoot : null;
}
