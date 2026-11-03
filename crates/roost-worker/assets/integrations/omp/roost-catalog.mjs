// Roost's ACP startup barrier; ordinary OMP extensions remain enabled.
// Await OMP's own provider discovery before publishing selectable models.
// The worker verifies the per-process result before accepting SessionReady.
import { writeFileSync } from "node:fs";

export default function (omp) {
  omp.on("session_start", async (_event, context) => {
    const statusPath = process.env.ROOST_OMP_CATALOG_STATUS;
    try {
      await context.modelRegistry.awaitBackgroundRefresh();
      await context.modelRegistry.refreshRuntimeProviders();
      writeFileSync(statusPath, JSON.stringify({ ready: true }));
    } catch (error) {
      writeFileSync(statusPath, JSON.stringify({ error: String(error) }));
      throw error;
    }
  });
}
