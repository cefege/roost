// Pins what `roost status` can honestly say about the browser build: the dist
// the installed unit stamped, whether that path still holds one, and whether
// the coordinator's own listener actually answers a page request — the state
// that otherwise shows up only as a 404 on every URL.
import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { _probeSpaRoot, resolveSpaStatus, resolveStatusEndpoint } from "../src/status.ts";
import { renderedStatusLines, statusReportFixture, testFetch } from "./status-render-fixture.ts";

const RETIRED_DIST = "/var/roost/releases/deleted-release/apps/web/dist";

describe("status spa reporting", () => {
  test("reads the stamped dist and reports the listener's own answer", async () => {
    const workdir = mkdtempSync(join(tmpdir(), "roost-status-spa-"));
    try {
      const dist = join(workdir, "apps", "web", "dist");
      mkdirSync(dist, { recursive: true });
      writeFileSync(join(dist, "index.html"), "<!doctype html>");
      const requested: string[] = [];
      const serving = testFetch(async (input, init) => {
        requested.push(`${init?.method} ${String(input)}`);
        return new Response(null, { status: 200 });
      });

      expect(await resolveSpaStatus(
        `[Service]\nEnvironment="ROOST_WEB_DIST_PATH=${dist}"`,
        "http://127.0.0.1:4103",
        "linux",
        serving,
      )).toEqual({ serves: true, webDistPath: dist, webDistPresent: true });
      // A page request, not a body download: the status is in the code.
      expect(requested).toEqual(["HEAD http://127.0.0.1:4103/"]);
    } finally {
      rmSync(workdir, { recursive: true, force: true });
    }
  });

  test("a stamped dist a release settlement deleted reports both facts", async () => {
    const missing = testFetch(async () => new Response("not found", { status: 404 }));

    expect(await resolveSpaStatus(
      `[Service]\nEnvironment="ROOST_WEB_DIST_PATH=${RETIRED_DIST}"`,
      "http://127.0.0.1:4103",
      "linux",
      missing,
    )).toEqual({ serves: false, webDistPath: RETIRED_DIST, webDistPresent: false });
  });

  test("a compiled install serving its embedded build is not called missing", async () => {
    // The CLI cannot read that install's embedded manifest, so the served
    // answer — not the stale path — decides.
    const serving = testFetch(async () => new Response(null, { status: 200 }));

    expect(await resolveSpaStatus(
      `[Service]\nEnvironment="ROOST_WEB_DIST_PATH=${RETIRED_DIST}"`,
      "http://127.0.0.1:4103",
      "linux",
      serving,
    )).toEqual({ serves: true, webDistPath: RETIRED_DIST, webDistPresent: false });
  });

  test("no listener to ask is unknown, never a missing claim", async () => {
    expect(await _probeSpaRoot(null)).toBeNull();
    expect(await _probeSpaRoot(
      "http://127.0.0.1:4103",
      testFetch(async () => { throw new Error("connection refused"); }),
    )).toBeNull();
  });

  test("the report probes the loopback bind, never the front door", async () => {
    // A front door behind Cloudflare Access 404s any request coord cannot see
    // as on-host, so probing it would call a healthy install missing. Off a
    // coordinator host there is no bind, and the answer is unknown.
    const probed: string[] = [];
    const record = testFetch(async (input) => {
      probed.push(String(input));
      return new Response(null, { status: 404 });
    });
    const unit = [
      "[Service]",
      'Environment="ROOST_COORDINATOR_BIND=127.0.0.1:4103"',
      'Environment="ROOST_WEB_PUBLIC_URL=https://dash.example.test"',
    ].join("\n");

    // An installed unit: the bind is probed, the declared front door is not.
    const installed = resolveStatusEndpoint(unit, { platform: "linux" });
    await resolveSpaStatus(unit, installed.coordUrl, "linux", record);
    expect(probed).toEqual(["http://127.0.0.1:4103/"]);

    // Off a coordinator host there is no bind, so nothing is probed and the
    // front door's Access 404 can never be read as a missing build.
    const offHost = resolveStatusEndpoint(null, {
      platform: "linux",
      override: { origin: "https://dash.example.test" },
    });
    expect(offHost.coordUrl).toBeNull();
    expect(await resolveSpaStatus(null, offHost.coordUrl, "linux", record)).toEqual({
      serves: null,
      webDistPath: null,
      webDistPresent: false,
    });
    expect(probed).toEqual(["http://127.0.0.1:4103/"]);
  });

  test("prints the served path, and a missing build with its cause and remedy", () => {
    expect(renderedStatusLines(statusReportFixture()))
      .toContain("  ✓ spa: served (/repo/apps/web/dist)");

    const retired = renderedStatusLines(statusReportFixture({
      spa: { serves: false, webDistPath: RETIRED_DIST, webDistPresent: false },
    }));
    expect(retired).toContain(`  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=${RETIRED_DIST} has no index.html)`);
    expect(retired.join("\n")).toContain("every page answers 404 while the API still works");

    const present = renderedStatusLines(statusReportFixture({
      spa: { serves: false, webDistPath: RETIRED_DIST, webDistPresent: true },
    }));
    expect(present).toContain(
      `  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=${RETIRED_DIST} exists but the coordinator serves no page)`,
    );

    expect(renderedStatusLines(statusReportFixture({
      spa: { serves: null, webDistPath: null, webDistPresent: false },
    }))).toContain("  - spa: not probed (no coordinator listener on this host)");
  });
});
