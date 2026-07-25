// Refreshes apps/web/src/vendor/omp-tool-views.js from the installed omp package.
// The bundle is omp's generated browser IIFE that registers <omp-tool-view> and
// self-injects its stylesheet; ToolCard.tsx mounts that element. Dev convenience
// only — the vendored copy is checked in, so CI and `vite build` never run this.
//
//   bun apps/web/scripts/sync-omp-tool-views.ts [/path/to/pi-coding-agent]

import { existsSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const OUT = join(import.meta.dir, "../src/vendor/omp-tool-views.js");
const REL_BUNDLE = "src/export/html/tool-views.generated.js";

function resolvePackageRoot(): string {
  const override = process.argv[2];
  if (override) return override;
  // <bun global bin>/omp → …/@oh-my-pi/pi-coding-agent/dist/cli.js → package root
  const bin = Bun.which("omp");
  if (!bin) {
    console.error("omp not on PATH; pass the package root explicitly: bun apps/web/scripts/sync-omp-tool-views.ts <root>");
    process.exit(1);
  }
  return dirname(dirname(realpathSync(bin)));
}

const root = resolvePackageRoot();
const src = join(root, REL_BUNDLE);
if (!existsSync(src)) {
  console.error(`tool-views bundle missing: ${src}`);
  process.exit(1);
}

const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version as string;
const header = `// vendored from @oh-my-pi/pi-coding-agent@${version} — regenerate with: bun apps/web/scripts/sync-omp-tool-views.ts\n`;
writeFileSync(OUT, header + readFileSync(src, "utf8"));
console.log(`wrote ${OUT} from ${src} (omp ${version})`);
