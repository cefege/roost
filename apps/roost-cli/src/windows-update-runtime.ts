import { probeServiceHealth } from "@roost/shared/service-health";
import { runWindowsHelper } from "@roost/shared/windows-helper";
import { WINDOWS_SERVICE_NAMES } from "./service-ctl.ts";
import type { ServiceHealthProver, WindowsUpdateNative } from "./windows-update-broker.ts";
import type { WindowsUpdateJournalV1 } from "./windows-update-journal.ts";

/** Narrow wrappers around the four updater operations implemented by the
 * pinned roost-win-helper. Arguments remain an argv vector end to end. */
export function createWindowsUpdateNative(): WindowsUpdateNative {
  return {
    async assertUpdaterServiceContext(): Promise<void> {
      assertWin32();
      await runWindowsHelper<void>("assert-service-context", [WINDOWS_SERVICE_NAMES.updater, String(process.pid)]);
    },
    async verifyCmsDetached(manifestPath, signaturePath, publisherSha256): Promise<void> {
      assertWin32();
      await runWindowsHelper<void>("verify-cms-detached", [manifestPath, signaturePath, "--publisher-sha256", publisherSha256]);
    },
    async verifyAuthenticode(path, publisherSha256): Promise<void> {
      assertWin32();
      await runWindowsHelper<void>("verify-authenticode", [path, "--publisher-sha256", publisherSha256]);
    },
    async extractZip(packagePath, destination, files): Promise<void> {
      assertWin32();
      const allowlist = new TextEncoder().encode(JSON.stringify({
        files: files.map(({ path, size, sha256 }) => ({ path, size, sha256 })),
      }));
      await runWindowsHelper<void>("extract-zip", [packagePath, destination], { input: allowlist });
    },
  };
}

export interface WindowsServiceHealthDescriptor {
  version: string;
  build: string;
  processEpoch: string;
  dbReady?: boolean;
  listenerReady?: boolean;
  targetLinkReady?: boolean;
  coordinatorUrl?: string;
}

/** WindowsCore supplies a DACL-protected LocalEndpoint reader. Keeping that
 * dependency structural prevents the updater from inventing a second IPC or
 * capability-file implementation. */
export interface WindowsLocalEndpointHealth {
  read(role: "worker" | "coordinator"): Promise<WindowsServiceHealthDescriptor>;
}

export function createServiceHealthProver(endpoint: WindowsLocalEndpointHealth = {
  read: async (role) => await probeServiceHealth(role),
}): ServiceHealthProver {
  return {
    async prove(role: "worker" | "coordinator", journal: Readonly<WindowsUpdateJournalV1>, mode: "forward" | "rollback"): Promise<void> {
      const health = await endpoint.read(role);
      const prior = journal.healthBefore[role];
      if (!prior) throw new Error(`${role} has no pre-update health checkpoint`);
      const expectedVersion = mode === "forward" ? journal.targetVersion : prior.version;
      if (normalizeVersion(health.version) !== normalizeVersion(expectedVersion) || !health.build || !health.processEpoch) {
        throw new Error(`${role} endpoint does not prove the expected version/build/process epoch`);
      }
      if (mode === "forward" && health.processEpoch === prior.processEpoch) {
        throw new Error(`${role} endpoint did not advance to a new process generation`);
      }
      if (mode === "rollback" && health.build !== prior.build) {
        throw new Error(`${role} rollback endpoint does not match the prior build`);
      }
      if (mode === "rollback" && rollbackRestarted(journal) && health.processEpoch === prior.processEpoch) {
        throw new Error(`${role} rollback endpoint did not advance to a restored process generation`);
      }
      if (role === "coordinator" && (!health.dbReady || !health.listenerReady)) {
        throw new Error("coordinator endpoint is not database/listener ready");
      }
      if (role === "worker" && (!health.targetLinkReady || !health.coordinatorUrl || health.coordinatorUrl !== prior.coordinatorUrl)) {
        throw new Error("worker endpoint has not reconnected to its prior coordinator target");
      }
    },
  };
}

function normalizeVersion(version: string): string {
  return version.replace(/^v/, "").split("+")[0];
}

function rollbackRestarted(journal: Readonly<WindowsUpdateJournalV1>): boolean {
  const order = ["prepared", "broker-started", "assets-staged", "services-stopped", "service-configs-switched", "current-manifest-switched", "services-restored", "health-proven", "committed"];
  return order.indexOf(journal.failure?.forwardPhase ?? "prepared") >= order.indexOf("services-stopped");
}

function assertWin32(): void {
  if (process.platform !== "win32") throw new Error(`Windows updater native operation refused on ${process.platform}`);
}
