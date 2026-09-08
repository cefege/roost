// Native status probes share the bounded subprocess path in this module.
// Report assembly uses the service-manager probe from here so a wedged
// launchctl, systemctl, or Windows helper cannot hang the one-shot readout.

import { runWindowsHelperSync, type WindowsServiceSnapshot } from "@roost/shared/windows-helper";
import { coordServiceLabel, workerServiceLabel } from "@roost/shared/paths";
import {
  COORD_UNIT,
  WINDOWS_SERVICE_NAMES,
  WORKER_UNIT,
} from "./service-ctl.ts";

const COORD_LABEL = coordServiceLabel();
const WORKER_LABEL = workerServiceLabel();

// launchctl/systemctl probes must never hang the one-shot status readout on a
// wedged service manager; a timed-out probe reports exitCode null, which folds
// into generic non-zero failure below.
const PROBE_DEADLINE_MS = 5_000;

export function captureStatusCommand(cmd: string[]): { exit: number; stdout: string } {
  try {
    const r = Bun.spawnSync(cmd, { timeout: PROBE_DEADLINE_MS });
    return { exit: r.exitCode ?? 1, stdout: r.stdout.toString() };
  } catch {
    return { exit: 127, stdout: "" };
  }
}

/** Worker/coord service running under the native platform manager? */
export function statusServiceLoaded(label: string): boolean {
  const worker = label === WORKER_LABEL;
  switch (process.platform) {
    case "linux":
      return captureStatusCommand([
        "systemctl",
        "--user",
        "is-active",
        worker ? WORKER_UNIT : COORD_UNIT,
      ]).exit === 0;
    case "darwin": {
      const uid = process.getuid?.() ?? "";
      return captureStatusCommand(["launchctl", "print", `gui/${uid}/${label}`]).exit === 0;
    }
    case "win32": {
      try {
        const service = runWindowsHelperSync<WindowsServiceSnapshot>(
          "service-query",
          [worker ? WINDOWS_SERVICE_NAMES.worker : WINDOWS_SERVICE_NAMES.coordinator, "basic"],
        );
        return service.state === "running";
      } catch {
        return false;
      }
    }
    default:
      throw new Error(`unsupported status platform: ${process.platform}`);
  }
}

export { COORD_LABEL as STATUS_COORD_LABEL, WORKER_LABEL as STATUS_WORKER_LABEL };
