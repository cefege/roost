import {
  probeServiceHealth,
  type ServiceHealthRole,
  type ServiceHealthStatusFor,
} from "@roost/shared/service-health";
import {
  runWindowsHelper,
  windowsCopyUpdaterArtifact,
  windowsInspectUpdaterArtifact,
  windowsProbeExclusiveOpen,
  windowsReadUpdaterArtifact,
  windowsReplaceUpdaterArtifact,
} from "@roost/shared/windows-helper";
import { WINDOWS_SERVICE_NAMES } from "../service-ctl.ts";
import type { ServiceHealthProver, WindowsUpdateNative } from "./windows-update-broker.ts";
import type { WindowsUpdateJournalV1, WindowsUpdateJournalV2 } from "./windows-update-journal.ts";

/** Narrow wrappers around the updater operations implemented by the pinned
 * roost-win-helper. Arguments remain an argv vector end to end. */
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
    async probeExclusiveOpen(path): Promise<boolean> {
      assertWin32();
      return await windowsProbeExclusiveOpen(path);
    },
    async protectArtifacts(path): Promise<void> {
      assertWin32();
      await runWindowsHelper<void>(
        "apply-artifact-dacl",
        [path, `NT SERVICE\\${WINDOWS_SERVICE_NAMES.updater}`],
      );
    },
    async readArtifact(path, profile, maxBytes): Promise<Uint8Array> {
      assertWin32();
      return await windowsReadUpdaterArtifact(path, profile, maxBytes);
    },
    async replaceArtifact(path, profile, contents): Promise<void> {
      assertWin32();
      const bytes = typeof contents === "string" ? Buffer.from(contents, "utf8") : contents;
      await windowsReplaceUpdaterArtifact(path, profile, bytes);
    },
    async copyArtifact(
      sourcePath,
      destinationPath,
      sourceProfile,
      destinationProfile,
      expected,
    ) {
      assertWin32();
      return await windowsCopyUpdaterArtifact(
        sourcePath,
        destinationPath,
        sourceProfile,
        destinationProfile,
        expected,
      );
    },
    async inspectArtifact(path, profile, expected) {
      assertWin32();
      return await windowsInspectUpdaterArtifact(path, profile, expected);
    },
  };
}

export type WindowsServiceHealthDescriptor<
  R extends ServiceHealthRole = ServiceHealthRole,
> = ServiceHealthStatusFor<R>;

/** WindowsCore supplies a DACL-protected LocalEndpoint reader. Keeping that
 * dependency structural prevents the updater from inventing a second IPC or
 * capability-file implementation. */
export interface WindowsLocalEndpointHealth {
  read<R extends ServiceHealthRole>(role: R): Promise<WindowsServiceHealthDescriptor<R>>;
}

export function createServiceHealthProver(
  endpoint: WindowsLocalEndpointHealth = {
    read: async <R extends ServiceHealthRole>(role: R) => await probeServiceHealth(role),
  },
  options: {
    timeoutMs?: number;
    retryMs?: number;
    now?: () => number;
    sleep?: (milliseconds: number) => Promise<void>;
  } = {},
): ServiceHealthProver {
  const timeoutMs = options.timeoutMs ?? 30_000;
  const retryMs = options.retryMs ?? 250;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? Bun.sleep;
  return {
    async read<R extends ServiceHealthRole>(
      role: R,
    ): Promise<WindowsServiceHealthDescriptor<R>> {
      return await endpoint.read(role);
    },
    async prove(
      role: ServiceHealthRole,
      journal: Readonly<WindowsUpdateJournalV1 | WindowsUpdateJournalV2>,
      mode: "forward" | "proof" | "rollback",
    ): Promise<void> {
      const deadline = now() + timeoutMs;
      let lastError: unknown;
      while (true) {
        try {
          assertExpectedHealth(role, await endpoint.read(role), journal, mode);
          return;
        } catch (error) {
          lastError = error;
          if (now() >= deadline) {
            throw new Error(`${role} did not prove ${mode} health within ${timeoutMs}ms: ${String(lastError)}`);
          }
          await sleep(retryMs);
        }
      }
    },
  };
}

function assertExpectedHealth(
  role: ServiceHealthRole,
  health: WindowsServiceHealthDescriptor,
  journal: Readonly<WindowsUpdateJournalV1 | WindowsUpdateJournalV2>,
  mode: "forward" | "proof" | "rollback",
): void {
  if (health.role !== role) throw new Error(`${role} endpoint returned mismatched role health`);
  const prior = journal.healthBefore[role];
  if (!prior) throw new Error(`${role} has no pre-update health checkpoint`);
  const expectedVersion = mode === "rollback" ? prior.version : journal.targetVersion;
  const expectedBuild = mode === "rollback" ? prior.build : journal.targetBuild;
  if (!expectedBuild) throw new Error(`${role} journal has no immutable ${mode} build identity`);
  if (
    normalizeVersion(health.version) !== normalizeVersion(expectedVersion)
    || health.build !== expectedBuild
    || !health.processEpoch
  ) {
    throw new Error(`${role} endpoint does not prove the exact expected version/build/process epoch`);
  }
  if (mode === "forward" && health.processEpoch === prior.processEpoch) {
    throw new Error(`${role} endpoint did not advance to a new process generation`);
  }
  if (mode === "proof" && health.processEpoch !== prior.processEpoch) {
    throw new Error(`${role} same-release endpoint changed process generation`);
  }

  if (mode === "rollback" && rollbackRestarted(journal) && health.processEpoch === prior.processEpoch) {
    throw new Error(`${role} rollback endpoint did not advance to a restored process generation`);
  }
  if (health.role === "coordinator" && (!health.dbReady || !health.listenerReady)) {
    throw new Error("coordinator endpoint is not database/listener ready");
  }
  if (
    health.role === "worker"
    && (!health.targetLinkReady || !health.coordinatorUrl || health.coordinatorUrl !== prior.coordinatorUrl)
  ) {
    throw new Error("worker endpoint has not reconnected to its prior coordinator target");
  }
}

function normalizeVersion(version: string): string {
  return version.replace(/^v/, "").split("+")[0];
}

function rollbackRestarted(
  journal: Readonly<WindowsUpdateJournalV1 | WindowsUpdateJournalV2>,
): boolean {
  const order = [
    "prepared",
    "broker-started",
    "assets-staged",
    "stable-artifacts-snapshotted",
    "services-stopped",
    "stable-artifacts-promoted",
    "current-manifest-switched",
    "services-restored",
    "health-proven",
    "committed",
    "cleanup-complete",
  ];
  return order.indexOf(journal.failure?.forwardPhase ?? "prepared") >= order.indexOf("services-stopped");
}

function assertWin32(): void {
  if (process.platform !== "win32") throw new Error(`Windows updater native operation refused on ${process.platform}`);
}
