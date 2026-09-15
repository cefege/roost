import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { normalizeManifestUrlKey } from "../../../scripts/gen-embed.ts";
import { createSpaResponder } from "../src/spa.ts";


describe("createSpaResponder", () => {
  test("uses a valid disk build without mixing embedded assets", async () => {
    const workdir = mkdtempSync(join(tmpdir(), "roost-spa-"));
    try {
      const diskRoot = join(workdir, "disk");
      const embeddedRoot = join(workdir, "embedded");
      mkdirSync(join(diskRoot, "assets"), { recursive: true });
      mkdirSync(join(embeddedRoot, "assets"), { recursive: true });
      writeFileSync(join(diskRoot, "index.html"), "disk index");
      writeFileSync(join(diskRoot, "assets", "disk.js"), "disk bundle");
      writeFileSync(join(embeddedRoot, "index.html"), "embedded index");
      writeFileSync(join(embeddedRoot, "assets", "embedded.js"), "embedded bundle");

      const responder = createSpaResponder(diskRoot, new Map([
        ["index.html", { raw: join(embeddedRoot, "index.html") }],
        ["assets/embedded.js", { raw: join(embeddedRoot, "assets", "embedded.js") }],
      ]));

      const root = await responder(new URL("http://t/"), "GET", "");
      expect(root.status).toBe(200);
      expect(await root.text()).toBe("disk index");

      const route = await responder(new URL("http://t/s/example"), "GET", "");
      expect(route.status).toBe(200);
      expect(await route.text()).toBe("disk index");

      const diskAsset = await responder(new URL("http://t/assets/disk.js"), "GET", "");
      expect(diskAsset.status).toBe(200);
      expect(await diskAsset.text()).toBe("disk bundle");

      const embeddedOnlyAsset = await responder(new URL("http://t/assets/embedded.js"), "GET", "");
      expect(embeddedOnlyAsset.status).toBe(404);
    } finally {
      rmSync(workdir, { recursive: true, force: true });
    }
  });

  test("falls back wholly to embedded assets when disk index is missing", async () => {
    const workdir = mkdtempSync(join(tmpdir(), "roost-spa-"));
    try {
      const embeddedRoot = join(workdir, "embedded");
      const diskRoot = join(workdir, "missing-index");
      mkdirSync(join(diskRoot, "assets"), { recursive: true });
      writeFileSync(join(diskRoot, "assets", "disk-only.js"), "disk-only bundle");
      mkdirSync(join(embeddedRoot, "assets"), { recursive: true });
      writeFileSync(join(embeddedRoot, "index.html"), "embedded index");
      writeFileSync(join(embeddedRoot, "assets", "embedded.js"), "embedded bundle");
      const responder = createSpaResponder(diskRoot, new Map([
        ["index.html", { raw: join(embeddedRoot, "index.html") }],
        ["assets/embedded.js", { raw: join(embeddedRoot, "assets", "embedded.js") }],
      ]));

      const root = await responder(new URL("http://t/"), "GET", "");
      expect(root.status).toBe(200);
      expect(await root.text()).toBe("embedded index");

      const asset = await responder(new URL("http://t/assets/embedded.js"), "GET", "");
      expect(asset.status).toBe(200);
      expect(await asset.text()).toBe("embedded bundle");

      expect((await responder(new URL("http://t/"), "POST", "")).status).toBe(405);
      expect((await responder(new URL("http://t/assets/disk-only.js"), "GET", "")).status).toBe(404);
    } finally {
      rmSync(workdir, { recursive: true, force: true });
    }
  });

  test("does not resolve disk assets outside the SPA root", async () => {
    const workdir = mkdtempSync(join(tmpdir(), "roost-spa-"));
    try {
      const diskRoot = join(workdir, "disk");
      const outsideRoot = join(workdir, "outside");
      mkdirSync(diskRoot, { recursive: true });
      mkdirSync(outsideRoot, { recursive: true });
      writeFileSync(join(diskRoot, "index.html"), "disk index");
      writeFileSync(join(outsideRoot, "secret.js"), "secret");
      const responder = createSpaResponder(diskRoot, new Map());
      const traversalUrl = { pathname: "/assets/../../outside/secret.js" } as URL;

      const response = await responder(traversalUrl, "GET", "");
      expect(response.status).toBe(404);
    } finally {
      rmSync(workdir, { recursive: true, force: true });
    }
  });

  test("serves embedded gzip bytes directly and preserves raw and HEAD semantics", async () => {
    const workdir = mkdtempSync(join(tmpdir(), "roost-spa-"));
    try {
      const rawIndex = join(workdir, "index.html");
      const rawAsset = join(workdir, "app.js");
      const gzipAsset = join(workdir, "app.js.gz");
      writeFileSync(rawIndex, "embedded index");
      writeFileSync(rawAsset, "raw javascript");
      // Deliberately not a valid gzip stream: the responder must serve the
      // generated variant as-is rather than recompressing the raw file.
      writeFileSync(gzipAsset, "build-time gzip bytes");
      const responder = createSpaResponder(undefined, new Map([
        ["index.html", { raw: rawIndex }],
        ["assets/app-deadbeef.js", { raw: rawAsset, gzip: gzipAsset }],
      ]));

      const compressed = await responder(new URL("http://t/assets/app-deadbeef.js"), "GET", "br, gzip");
      expect(compressed.status).toBe(200);
      expect(compressed.headers.get("content-encoding")).toBe("gzip");
      expect(compressed.headers.get("vary")).toBe("accept-encoding");
      expect(compressed.headers.get("content-type")).toBe("application/javascript; charset=utf-8");
      expect(compressed.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
      expect(await compressed.text()).toBe("build-time gzip bytes");

      const raw = await responder(new URL("http://t/assets/app-deadbeef.js"), "GET", "gzip;q=0, br");
      expect(raw.headers.get("content-encoding")).toBeNull();
      expect(raw.headers.get("vary")).toBe("accept-encoding");
      expect(await raw.text()).toBe("raw javascript");

      const head = await responder(new URL("http://t/assets/app-deadbeef.js"), "HEAD", "gzip");
      expect(head.headers.get("content-encoding")).toBe("gzip");
      expect(head.headers.get("vary")).toBe("accept-encoding");
      expect(await head.text()).toBe("");
    } finally {
      rmSync(workdir, { recursive: true, force: true });
    }
  });

  test("normalizes Windows build paths into URL manifest keys", () => {
    expect(normalizeManifestUrlKey("assets\\chunks\\terminal-deadbeef.js"))
      .toBe("assets/chunks/terminal-deadbeef.js");
    expect(normalizeManifestUrlKey("assets/chunks/terminal-deadbeef.js"))
      .toBe("assets/chunks/terminal-deadbeef.js");
  });
});
