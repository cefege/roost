// Renderer qualification server — bundles and serves one loopback-only fixture.
// qualify.ts drives its URL; no production SPA, PTY, coordinator, or worker starts.
// Bundled assets remain in memory so generated files cannot escape the experiment.
// Unknown paths fail closed instead of falling through to application assets.

const fixtureRoot = new URL("./", import.meta.url);
const buildConfig = {
  entrypoints: [new URL("./browser.js", fixtureRoot).pathname],
  target: "browser" as const,
  format: "esm" as const,
  splitting: false,
  write: false,
  outdir: ".cache/renderer-qualification/bundle",
  naming: "[name]-[hash].[ext]",
};
const build = await Bun.build(buildConfig);
if (!build.success) {
  const messages = build.logs.map((entry) => entry.message).join("\n");
  throw new Error(`RendererQualificationBuildFailed:${messages}`);
}

const assets = new Map<string, Response>();
let browserScriptPath = "";
let stylesheetPath = "";
for (const output of build.outputs) {
  const pathname = `/assets/${output.path.split("/").at(-1)}`;
  const contentType = pathname.endsWith(".js") ? "text/javascript; charset=utf-8"
    : pathname.endsWith(".css") ? "text/css; charset=utf-8"
      : "application/octet-stream";
  assets.set(pathname, new Response(output, { headers: { "content-type": contentType } }));
  if (pathname.endsWith(".js")) browserScriptPath = pathname;
  if (pathname.endsWith(".css")) stylesheetPath = pathname;
}
if (!browserScriptPath || !stylesheetPath) throw new Error("RendererQualificationAssetMissing");

const html = `<!doctype html>
<html><head><meta charset="utf-8"><link rel="stylesheet" href="${stylesheetPath}">
<style>
body { margin: 0; background: #151515; color: #eee; }
#qualification-surface { width: 720px; height: 360px; overflow: auto; font: 16px/18px monospace; }
#qualification-grid { min-width: max-content; --term-row-height: 18px; }
#qualification-notice { min-height: 18px; font: 14px/18px monospace; }
</style></head><body>
<div id="qualification-notice" aria-live="polite"></div>
<div id="qualification-surface" class="wterm" data-row-height="18" data-char-width="9"><div id="qualification-grid" class="term-grid"></div></div>
<script type="module" src="${browserScriptPath}"></script></body></html>`;

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    const pathname = new URL(request.url).pathname;
    if (pathname === "/") return new Response(html, { headers: { "content-type": "text/html; charset=utf-8" } });
    const asset = assets.get(pathname);
    return asset?.clone() ?? new Response("Not found", { status: 404 });
  },
});
console.log(`RENDERER_QUALIFICATION_READY http://127.0.0.1:${server.port}`);
