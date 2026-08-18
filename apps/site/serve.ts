// Static file server for the built marketing site (apps/site/dist).
// No dependencies: run with `bun serve.ts` (see README.md for the systemd unit).
import { join, resolve, sep } from "node:path";

const PORT = Number(process.env.ROOST_SITE_PORT ?? 4180);
const HOST = process.env.ROOST_SITE_HOST ?? "127.0.0.1";
const ROOT = resolve(import.meta.dir, "dist");
const NOT_FOUND_HTML = join(ROOT, "404.html");

const IMMUTABLE = "public, max-age=31536000, immutable";
const REVALIDATE = "public, max-age=300";

/** Resolve a request pathname inside ROOT, or null if it escapes / is malformed. */
function resolveInsideRoot(pathname: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return null; // malformed percent-encoding
  }
  if (decoded.includes("\0")) return null;
  // Force the path to be interpreted relative to ROOT, then prove containment:
  // this catches "..", encoded "%2e%2e", and absolute-looking paths alike.
  const rel = decoded.startsWith("/") ? decoded.slice(1) : decoded;
  const candidate = resolve(ROOT, `./${rel}`);
  if (candidate !== ROOT && !candidate.startsWith(ROOT + sep)) return null;
  return candidate;
}

async function pickFile(candidate: string, pathname: string) {
  if (!pathname.endsWith("/")) {
    const direct = Bun.file(candidate);
    if (await direct.exists()) return direct;
  }
  const index = Bun.file(join(candidate, "index.html"));
  if (await index.exists()) return index;
  return null;
}

async function notFound(): Promise<Response> {
  const page = Bun.file(NOT_FOUND_HTML);
  if (await page.exists()) {
    return new Response(page, {
      status: 404,
      headers: { "Cache-Control": REVALIDATE },
    });
  }
  return new Response("404 Not Found\n", {
    status: 404,
    headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": REVALIDATE },
  });
}

async function handle(req: Request, pathname: string): Promise<Response> {
  if (req.method !== "GET" && req.method !== "HEAD") {
    return new Response("405 Method Not Allowed\n", {
      status: 405,
      headers: { Allow: "GET, HEAD", "Content-Type": "text/plain; charset=utf-8" },
    });
  }
  const candidate = resolveInsideRoot(pathname);
  if (candidate === null) {
    return new Response("403 Forbidden\n", {
      status: 403,
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    });
  }
  const file = await pickFile(candidate, pathname);
  if (!file) return notFound();
  return new Response(file, {
    headers: { "Cache-Control": pathname.startsWith("/_astro/") ? IMMUTABLE : REVALIDATE },
  });
}

const server = Bun.serve({
  port: PORT,
  hostname: HOST,
  async fetch(req) {
    const started = performance.now();
    const pathname = new URL(req.url).pathname;
    const res = await handle(req, pathname);
    const ms = (performance.now() - started).toFixed(1);
    console.log(`${req.method} ${pathname} ${res.status} ${ms}ms`);
    return res;
  },
});

console.log(`roost-site serving ${ROOT} on http://${server.hostname}:${server.port}`);
