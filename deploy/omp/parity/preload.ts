import "./network-guard";
import { plugin } from "bun";
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import type { InterfaceInventory } from "./inventory-types";
import { instrumentSource } from "./probe-transform";
import { probe } from "./probe";
import { installLiveTransportReplay } from "./live-transport-replay";
import { installFixtureTransportReplay } from "./fixture-transport-replay";

installLiveTransportReplay();
// Declared fixture services (Smithery, Codex auth, OAuth callbacks) bind before any source module loads.
installFixtureTransportReplay();

const repository = resolve(import.meta.dir, "../../..");
const artifactRoot = join(repository, "target/omp-parity");
const sourceRoot = realpathSync(process.env.OMP_PARITY_SOURCE_ROOT ?? join(repository, "target/omp-parity-source"));
const ownedSource = join(repository, "target/omp-parity-source");
if (lstatSync(ownedSource).isSymbolicLink() || sourceRoot !== realpathSync(ownedSource)) {
  throw new Error("CLI instrumentation may only read the owned parity source checkout.");
}
const inventoryPath = resolve(process.env.OMP_PARITY_INVENTORY ?? join(artifactRoot, "inventory.json"));
const inventoryRelative = relative(realpathSync(artifactRoot), realpathSync(inventoryPath));
if (inventoryRelative === ".." || inventoryRelative.startsWith("../") || isAbsolute(inventoryRelative)) {
  throw new Error("OMP_PARITY_INVENTORY must be an owned inventory artifact under target/omp-parity.");
}
const inventory = JSON.parse(readFileSync(inventoryPath, "utf8")) as InterfaceInventory;
if (!inventory || !Array.isArray(inventory.entries) || inventory.entries.length === 0) {
  throw new Error("Actual source-derived OMP inventory is required before starting the CLI probe.");
}
probe.requireInstrumentation();
const probeModule = pathToFileURL(join(import.meta.dir, "probe.ts")).href;

plugin({
  name: "owned-omp-runtime-provenance",
  setup(builder) {
    // Bun's runtime onLoad cannot decline a matched file, so the filter itself selects the owned source tree.
    // The callback stays synchronous: an async onLoad makes the module async, and the CLI require()s some.
    const ownedPackages = new RegExp(`^${sourceRoot.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}/packages/.*\\.tsx?$`);
    builder.onLoad({ filter: ownedPackages }, args => {
      const canonical = realpathSync(args.path);
      const local = relative(sourceRoot, canonical);
      const loader = canonical.endsWith(".tsx") ? "tsx" : "ts";
      const text = readFileSync(canonical, "utf8");
      if (local === ".." || local.startsWith("../") || isAbsolute(local)) return { contents: text, loader };
      const source = local.split("\\").join("/");
      if (!source.startsWith("packages/") || source.includes("/node_modules/") || source.endsWith(".d.ts")) return { contents: text, loader };
      const transformed = instrumentSource(text, source, probeModule, inventory.entries);
      return { contents: transformed.contents, loader };
    });
  },
});
