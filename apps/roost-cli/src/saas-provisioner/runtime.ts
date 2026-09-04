/**
 * Builds the root-side provisioner runtime from its verification key, replay store, and worker.
 * The hidden provisioner command calls this module to bind the private Unix socket.
 * Verification-key checks and lazy worker loading preserve the gateway-to-provisioner trust split.
 */

import { constants, openSync, closeSync, fstatSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { openSshEd25519PublicKey, PRIVATE_IPC_SOCKET_PATH } from "../saas-auth/private-ipc.ts";
import { ProvisionerReplayStore } from "./replay-store.ts";
import { ProvisionerIpcServer } from "./server.ts";
import type { ProvisionerOperation } from "./server.ts";
import type { ProvisionerOperationRuntime } from "../saas/provisioner-operation.ts";


function readVerificationKey(path: string) {
  const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(descriptor);
    if (!stat.isFile() || stat.size <= 0 || stat.size > 4_096) throw new Error("provisioner verification key file is invalid");
    return openSshEd25519PublicKey(readFileSync(descriptor));
  } finally { closeSync(descriptor); }
}

export async function serveProvisioner(options: { operation?: ProvisionerOperation } = {}): Promise<void> {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) throw new Error("SaaS provisioner must run as root");
  const maximumAccounts = Number(process.env.ROOST_SAAS_MAX_ACCOUNTS);
  if (!Number.isSafeInteger(maximumAccounts) || maximumAccounts <= 0) throw new Error("ROOST_SAAS_MAX_ACCOUNTS must be a positive integer");
  const root = process.env.ROOST_SAAS_ROOT ?? "/srv/data/roost";
  const keyPath = process.env.ROOST_SAAS_AUTH_VERIFY_KEY_FILE;
  if (!keyPath) throw new Error("ROOST_SAAS_AUTH_VERIFY_KEY_FILE is required");
  let operation = options.operation;
  let operationRuntime: ProvisionerOperationRuntime | undefined;
  if (!operation) {
    // Tests inject an operation without loading the privileged deployment worker.
    const worker = await import("../saas-provisioner-worker.ts") as unknown as {
      createProvisionerOperation?: () => Promise<ProvisionerOperationRuntime>;
    };
    if (!worker.createProvisionerOperation) throw new Error("provisioner operation factory is unavailable");
    operationRuntime = await worker.createProvisionerOperation();
    operation = operationRuntime.operation;
  }
  const replayStore = new ProvisionerReplayStore({ path: process.env.ROOST_SAAS_CONTROL_DB ?? join(root, "control.db") });
  const server = new ProvisionerIpcServer({ socketPath: PRIVATE_IPC_SOCKET_PATH, verificationKey: readVerificationKey(keyPath), replayStore, operation });
  try {
    await server.listen();
    const { promise, resolve } = Promise.withResolvers<void>();
    const stop = () => resolve();
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    await promise;
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  } finally {
    try {
      await server.close();
    } finally {
      try {
        await operationRuntime?.close();
      } finally {
        replayStore.close();
      }
    }
  }
}
