import { afterEach, expect, test } from "bun:test";
import * as fs from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WorkerCoordRelocation } from "../src/coord-relocation.ts";

const roots: string[] = [];
afterEach(() => roots.splice(0).forEach((root) => fs.rmSync(root, { recursive: true, force: true })));

const HANDOFF = "00000000-0000-4000-8000-000000000001";
const OTHER_HANDOFF = "00000000-0000-4000-8000-000000000002";
const SOURCE = "https://source.ts.net:4102";
const TARGET = "https://target.ts.net:4102";

/** A temp root with a journal path and a realistic service definition for this
 *  platform, seeded so persistEndpoint() has something to rewrite and verify. */
function setup(seededUrl: string) {
  const root = fs.mkdtempSync(join(tmpdir(), "roost-coord-relocation-"));
  roots.push(root);
  const journalPath = join(root, "coord-relocation.json");
  const servicePath = join(root, process.platform === "darwin" ? "dev.roost.worker.plist" : "roost-worker.service");
  fs.writeFileSync(servicePath, process.platform === "darwin"
    ? `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
\t<key>Label</key>
\t<string>dev.roost.worker</string>
\t<key>ProgramArguments</key>
\t<array>
\t\t<string>/usr/local/bin/roost</string>
\t\t<string>worker</string>
\t</array>
\t<key>EnvironmentVariables</key>
\t<dict>
\t\t<key>ROOST_COORDINATOR_URL</key>
\t\t<string>${seededUrl}</string>
\t</dict>
\t<key>RunAtLoad</key>
\t<true/>
</dict>
</plist>
`
    : `[Unit]\nDescription=Roost worker\n\n[Service]\nEnvironment=ROOST_COORDINATOR_URL=${seededUrl}\nExecStart=/usr/local/bin/roost worker\n\n[Install]\nWantedBy=default.target\n`);
  return { root, journalPath, servicePath, relocation: new WorkerCoordRelocation(journalPath, servicePath) };
}

/** Reads the endpoint back out of the service definition the same way the
 *  platform's service manager would. */
async function readEndpoint(servicePath: string): Promise<string> {
  if (process.platform === "darwin") {
    const get = Bun.spawn(["/usr/libexec/PlistBuddy", "-c", "Print :EnvironmentVariables:ROOST_COORDINATOR_URL", servicePath], { stdout: "pipe", stderr: "ignore" });
    const value = (await new Response(get.stdout).text()).trim();
    expect(await get.exited).toBe(0);
    return value;
  }
  return fs.readFileSync(servicePath, "utf8")
    .match(/^Environment=(?:"ROOST_COORDINATOR_URL=((?:\\.|[^"])*)"|ROOST_COORDINATOR_URL=(.*))$/m)
    ?.slice(1).find((value) => value !== undefined)
    ?.replace(/\\"/g, "\"")
    .replace(/\\\\/g, "\\") ?? "";
}

test("a late abort leaves a committed worker pointed at the target", async () => {
  const { journalPath, servicePath, relocation } = setup(SOURCE);
  const committed: Array<[string, boolean | undefined]> = [];
  // Await the deferred relocate itself rather than a wall-clock tick; the
  // bail-out only fires if the callback never runs, so a regression is an
  // assertion failure instead of a suite hang.
  const fired = Promise.withResolvers<"fired">();
  const bail = Promise.withResolvers<"never fired">();
  relocation.activate({ handoff_id: HANDOFF, source_url: SOURCE, target_url: TARGET });
  await relocation.commit(() => 0, (url, force) => {
    committed.push([url, force]);
    fired.resolve("fired");
  });
  // commit() defers its relocate so handleDownstream's rpc-ok lands first.
  expect(committed).toEqual([]);
  const bailTimer = setTimeout(() => bail.resolve("never fired"), 1000);
  expect(await Promise.race([fired.promise, bail.promise])).toBe("fired");
  clearTimeout(bailTimer);
  expect(committed).toEqual([[TARGET, true]]);
  expect(await readEndpoint(servicePath)).toBe(TARGET);
  expect(relocation.load()?.state).toBe("COMMITTED");

  const journalAfterCommit = fs.readFileSync(journalPath, "utf8");
  const aborted: string[] = [];
  await relocation.abort(HANDOFF, (url) => aborted.push(url));

  expect(aborted).toEqual([]);
  expect(await readEndpoint(servicePath)).toBe(TARGET);
  expect(fs.existsSync(journalPath)).toBeTrue();
  expect(fs.readFileSync(journalPath, "utf8")).toBe(journalAfterCommit);
});

test("an abort for a different handoff is a complete no-op", async () => {
  const { journalPath, servicePath, relocation } = setup(SOURCE);
  relocation.stage({ handoff_id: HANDOFF, source_url: SOURCE, target_url: TARGET });
  const journalBefore = fs.readFileSync(journalPath, "utf8");
  const serviceBefore = fs.readFileSync(servicePath, "utf8");

  const aborted: string[] = [];
  await relocation.abort(OTHER_HANDOFF, (url) => aborted.push(url));

  expect(aborted).toEqual([]);
  expect(fs.existsSync(journalPath)).toBeTrue();
  expect(fs.readFileSync(journalPath, "utf8")).toBe(journalBefore);
  expect(fs.readFileSync(servicePath, "utf8")).toBe(serviceBefore);
  expect(relocation.load()?.state).toBe("STAGED");
});

test("aborting a staged handoff repoints the worker at the source and clears the journal", async () => {
  const { journalPath, servicePath, relocation } = setup(TARGET);
  relocation.stage({ handoff_id: HANDOFF, source_url: SOURCE, target_url: TARGET });

  const aborted: string[] = [];
  await relocation.abort(HANDOFF, (url) => aborted.push(url));

  expect(aborted).toEqual([SOURCE]);
  expect(await readEndpoint(servicePath)).toBe(SOURCE);
  expect(fs.existsSync(journalPath)).toBeFalse();
  expect(relocation.load()).toBeNull();
});
