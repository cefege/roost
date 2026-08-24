// Static responder for exactly one complete SPA build: a valid on-disk dist
// wins for source runs, otherwise the build-time embedded manifest — chosen
// once at creation so two generations can never be mixed in one process.
// Source-run disk assets are mutable, so their gzip bodies are memoized in a
// small cache invalidated on mtime/size mismatch; embedded assets skip that
// cache because their raw and gzip forms both ship at build time.
import { existsSync, statSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

// Text assets compressed at build time for embedded production builds and on
// demand only for mutable source-run builds.
const COMPRESSIBLE_EXT: Record<string, true> = {
  ".js": true, ".mjs": true, ".css": true, ".html": true, ".json": true, ".svg": true, ".map": true, ".txt": true,
  ".webmanifest": true,
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

// Source-run disk assets remain mutable, so their gzip bodies are memoized and
// invalidated on an mtime/size mismatch. Embedded production assets never use
// this cache: their raw and gzip files are both embedded at build time.
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

export interface EmbeddedSpaAsset {
  readonly raw: string;
  readonly gzip?: string;
}

function acceptsGzip(value: string): boolean {
  let wildcard = false;
  for (const item of value.split(",")) {
    const [codingPart, ...parameters] = item.split(";");
    const coding = codingPart?.trim().toLowerCase();
    let accepted = true;
    for (const parameter of parameters) {
      const match = /^\s*q\s*=\s*(\d*(?:\.\d+)?)\s*$/i.exec(parameter);
      if (match) accepted = Number(match[1]) > 0;
    }
    if (coding === "gzip") return accepted;
    if (coding === "*") wildcard = accepted;
  }
  return wildcard;
}

/**
 * Create a responder from exactly one complete SPA build. A valid on-disk
 * index wins for source runs; otherwise compiled installations use the
 * embedded manifest. Choosing once prevents mixing build generations.
 */
export function createSpaResponder(
  webDistPath: string | undefined,
  embeddedAssets: ReadonlyMap<string, EmbeddedSpaAsset>,
): (url: URL, method: string, acceptEncoding: string) => Promise<Response> {
  const spaRoot = diskSpaRoot(webDistPath);
  const webAssets = spaRoot || embeddedAssets.size === 0 ? null : embeddedAssets;

  // rel (no leading slash) → an embedded raw/gzip descriptor or disk path.
  function resolveAsset(rel: string): EmbeddedSpaAsset | null {
    if (!rel) return null;
    if (webAssets) return webAssets.get(rel) ?? null;
    if (!spaRoot) return null;
    const candidate = join(spaRoot, rel);
    const child = relative(spaRoot, candidate);
    const safe = child !== ".." && !child.startsWith(`..${sep}`) && !isAbsolute(child);
    return safe && existsSync(candidate) && statSync(candidate).isFile()
      ? { raw: candidate }
      : null;
  }

  function resolveIndex(): EmbeddedSpaAsset | null {
    if (webAssets) return webAssets.get("index.html") ?? null;
    return spaRoot ? { raw: join(spaRoot, "index.html") } : null;
  }

  async function fileResponse(
    asset: EmbeddedSpaAsset,
    rel: string,
    ext: string,
    method: string,
    acceptEncoding: string,
    isIndex: boolean,
    hashed: boolean,
    isEmbedded: boolean,
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
    const gzipAvailable = isEmbedded ? asset.gzip !== undefined : COMPRESSIBLE_EXT[ext] === true;
    if (gzipAvailable) headers["vary"] = "accept-encoding";
    if (gzipAvailable && acceptsGzip(acceptEncoding)) {
      headers["content-encoding"] = "gzip";
      if (method === "HEAD") return new Response(null, { status: 200, headers });
      const body = isEmbedded ? Bun.file(asset.gzip!) : await gzipCached(asset.raw);
      return new Response(body, { status: 200, headers });
    }
    return new Response(method === "HEAD" ? null : Bun.file(asset.raw), { status: 200, headers });
  }

  return async function spaResponse(url: URL, method: string, acceptEncoding: string): Promise<Response> {
    if (method !== "GET" && method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const rel = url.pathname.replace(/^\/+/, "");
    const asset = resolveAsset(rel);
    if (asset) {
      const dot = rel.lastIndexOf(".");
      return fileResponse(asset, rel, dot >= 0 ? rel.slice(dot) : "", method, acceptEncoding, false, rel.startsWith("assets/"), webAssets !== null);
    }
    if (rel.startsWith("assets/")) {
      return new Response("not found", { status: 404 });
    }
    const index = resolveIndex();
    if (index) return fileResponse(index, "index.html", ".html", method, acceptEncoding, true, false, webAssets !== null);
    return new Response("not found", { status: 404 });
  };
}

function diskSpaRoot(webDistPath: string | undefined): string | null {
  if (!webDistPath) return null;
  const spaRoot = resolve(webDistPath);
  const indexPath = join(spaRoot, "index.html");
  return existsSync(indexPath) && statSync(indexPath).isFile() ? spaRoot : null;
}
