// `roost coord` — run the coordinator in THIS process (the compiled binary's
// server mode; the analog of `herdr server`). runCoord auto-detects the SPA +
// migrations baked into the binary by scripts/build-binary.ts, and otherwise
// serves the SPA from disk and reads migrations from apps/coord/migrations.
import { runCoord } from "../../coord/src/main.ts";

export async function coord(_args: string[]): Promise<void> {
  await runCoord();
}
