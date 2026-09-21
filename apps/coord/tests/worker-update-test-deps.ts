// Shared inert worker-update dependencies for coordinator tests unrelated to
// deployment. Production always injects the initialized process-owned owner;
// focused deploy suites construct owners with their own durable fixtures.

import { WorkerUpdateOwner } from "../src/worker-update-owner.ts";

export function workerUpdateTestDeps(): {
  updateOwner: WorkerUpdateOwner;
  updateSourceRoot: string;
  activationGate: {
    updateReady(): boolean;
    updateTransactionId(): string | null;
    dispose(): void;
  };
} {
  return {
    updateSourceRoot: process.cwd(),
    activationGate: {
      updateReady: () => true,
      updateTransactionId: () => null,
      dispose: () => {},
    },
    updateOwner: new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.test",
      coordinatorDialUrl: "https://coord.test",
      readBaseline: async () => ({
        heartbeatAtMs: 0,
        processEpoch: null,
        gitSha: null,
        keeperPid: null,
        keeperEpoch: null,
        bindingDigest: null,
        sessionIds: [],
      }),
      readVerification: async () => null,
      publishOperation: () => {},
      workerRoutable: () => false,
      workerExists: async () => false,
    }),
  };
}
