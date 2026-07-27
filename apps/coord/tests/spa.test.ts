import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
        ["index.html", join(embeddedRoot, "index.html")],
        ["assets/embedded.js", join(embeddedRoot, "assets", "embedded.js")],
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
        ["index.html", join(embeddedRoot, "index.html")],
        ["assets/embedded.js", join(embeddedRoot, "assets", "embedded.js")],
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
});
