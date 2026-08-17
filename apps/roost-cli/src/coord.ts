// `roost coord` — run the coordinator in THIS process (the compiled binary's
// server mode; the analog of `herdr server`). runCoord auto-detects the SPA +
// migrations baked into the binary by scripts/build-binary.ts, and otherwise
// serves the SPA from disk and reads migrations from apps/coord/migrations.
import { roostServiceDir } from "@roost/shared/paths";
import { readWindowsRelocationRoleOverride } from "@roost/shared/windows-relocation";

export async function coord(_args: string[]): Promise<void> {
  if (process.platform === "win32") {
    const override = readWindowsRelocationRoleOverride(roostServiceDir(), "coordinator");
    if (override) {
      for (const [key, value] of Object.entries(override.environment)) process.env[key] = value;
    }
  }
  // A static import cannot work here: coordinator paths are resolved during
  // module initialization and must observe the protected override.
  const { runCoord } = await import("../../coord/src/main.ts");
  await runCoord();
}
