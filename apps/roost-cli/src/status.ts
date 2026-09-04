// This public entry keeps every `roost status` export on one stable path.
// Probing, report assembly, and rendering are split by responsibility so the
// command remains easy to navigate without changing its callers or output.

import { printStatusReport, statusReportIsHealthy } from "./status-output.ts";
import { statusReport } from "./status-report.ts";

export {
  ensureTailscale,
  resolveTailscale,
} from "./status-native-probes.ts";
export type { TailscalePreflightDeps } from "./status-native-probes.ts";
export {
  _probeCoordinatorIdentity,
  resolveCoordinatorDbPath,
  resolveStatusEndpoint,
  resolveTlsMode,
  workerInventory,
} from "./status-report.ts";
export {
  printStatusReport,
  statusReport,
  statusReportIsHealthy,
};
export type {
  HandoffStatus,
  ResolvedStatusEndpoint,
  StatusEndpointOverride,
  StatusEndpointResolverOptions,
  StatusReport,
  TailscaleStatus,
  WorkerStatus,
} from "./status-types.ts";

export async function status(_args: string[]): Promise<void> {
  const report = await statusReport();
  printStatusReport(report);
  // Non-zero exit when anything critical is down, so `roost status` is usable
  // as a scriptable gate (quickstart, CI, the install.sh tail).
  process.exit(statusReportIsHealthy(report) ? 0 : 1);
}
