// Pins ownership recognition for agent-integration assets already on disk.
// Installed assets carry their marker below a spliced report-transport
// preamble, so planning, adoption and the commit guard must all see a marker
// that sits ~100 lines into the file — and must still refuse unmarked files.
import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _loadAgentIntegrationAssets,
  installAgentIntegrations,
  resolveOmpExtensionDir,
} from "../src/agent-status/install-integrations.ts";
import {
  assertIntegrationTargetUnchanged,
  hasIntegrationOwnership,
  inspectIntegrationTarget,
} from "../src/agent-status/integration-install-proof.ts";

const OMP_MARKER = "ROOST_INTEGRATION_ID=omp";
const OMP_MARKER_COMMENT = `// ${OMP_MARKER} ROOST_INTEGRATION_VERSION=2`;
const cleanupDirs: string[] = [];

afterEach(async () => {
  await Promise.all(cleanupDirs.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

async function tempHome(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "roost-integration-ownership-"));
  cleanupDirs.push(path);
  return path;
}

/** An installed asset in the shape roost writes it: the shared report
 *  transport — a whole module, not a comment header — sits above the
 *  integration's own header, putting the trailing line on file line 106. */
function transportPrefixedAsset(headerLine: string): string {
  const transport = [
    "// Shared delivery transport for the agent-status integrations (omp + pi).",
    "// Spliced in so the installed file resolves no imports of its own.",
    'import net from "node:net";',
    ...Array.from({ length: 100 }, (_, idx) => `const transportStep${idx} = ${idx};`),
  ].join("\n");
  return `${transport}\n\n// Roost-owned integration.\n${headerLine}\nexport default function install(): void {}\n`;
}

describe("agent integration ownership marker", () => {
  test("is recognized in any comment line and nowhere else", () => {
    const installed = transportPrefixedAsset(OMP_MARKER_COMMENT);
    const markerLine =
      installed.split("\n").findIndex((line) => line.includes(OMP_MARKER)) + 1;
    expect(markerLine).toBe(106);
    expect(hasIntegrationOwnership(installed, OMP_MARKER)).toBe(true);

    expect(hasIntegrationOwnership(
      transportPrefixedAsset(`const claimed = "${OMP_MARKER}";`),
      OMP_MARKER,
    )).toBe(false);
    expect(hasIntegrationOwnership(
      transportPrefixedAsset("// ROOST_INTEGRATION_ID=omp-reference"),
      OMP_MARKER,
    )).toBe(false);
    expect(hasIntegrationOwnership(
      transportPrefixedAsset("// hand-written extension"),
      OMP_MARKER,
    )).toBe(false);
  });

  test("adopts and overwrites an installed asset marked below the file head", async () => {
    const home = await tempHome();
    const directory = resolveOmpExtensionDir({}, home);
    const target = join(directory, "roost-omp-agent-state.ts");
    await mkdir(directory, { recursive: true });
    await writeFile(target, transportPrefixedAsset(OMP_MARKER_COMMENT));

    const report = await installAgentIntegrations({}, home);

    const current = (await _loadAgentIntegrationAssets()).find(({ spec }) =>
      spec.id === "omp-status"
    );
    expect(await readFile(target, "utf8")).toBe(current!.content);
    expect(report.installed.map(({ id }) => id)).toContain("omp-status");
    expect(report.failed).toEqual([]);
  });

  test("passes the commit guard for a target marked below the file head", async () => {
    const home = await tempHome();
    const target = join(home, "roost-omp-agent-state.ts");
    await writeFile(target, transportPrefixedAsset(OMP_MARKER_COMMENT));
    const existing = await inspectIntegrationTarget(
      target,
      "agent integration target",
    );

    await expect(assertIntegrationTargetUnchanged({
      target,
      ownershipMarker: OMP_MARKER,
      existing,
    })).resolves.toBeUndefined();
  });
});
