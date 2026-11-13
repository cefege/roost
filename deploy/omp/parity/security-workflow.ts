import { randomUUID } from "node:crypto";
import { rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { ProfileManifest } from "./profile-manifest";
import { assertOwned } from "./profile-manifest";
import { objectValue } from "./builtin-capture-state";
import { securityWorkflowHooks } from "./builtin-security-workflow-requirements";

export interface SecurityControlRequest {
  id: string;
  scenarioId: string;
  checkpoint: number;
  phase: "before-input" | "after-input";
  expiresAt: number;
}
export interface SecurityControlResponse {
  id: string;
  scenarioId: string;
  checkpoint: number;
  phase: SecurityControlRequest["phase"];
  bindings?: Record<string, string>;
  error?: string;
}
export interface SecurityWorkflowClient {
  beforeInput(checkpoint: number): Promise<void>;
  afterInput(checkpoint: number): Promise<Record<string, string>>;
  requiredHeldProviderTurns(checkpoint: number): number[];
}

export function securityWorkflowPaths(home: string) {
  const directory = join(home, ".parity");
  return { request: join(directory, "security-control-request.json"), response: join(directory, "security-control-response.json"), bindings: join(directory, "security-bindings.json") };
}

export function createSecurityWorkflow(profile: Pick<ProfileManifest, "home">, scenarioId: string): SecurityWorkflowClient | undefined {
  const hook = securityWorkflowHooks[scenarioId];
  if (!hook) return undefined;
  const paths = securityWorkflowPaths(resolve(profile.home));
  for (const path of Object.values(paths)) assertOwned(profile.home, path);
  const bindings: Record<string, string> = {};
  let pending = false;
  async function exchange(checkpoint: number, phase: SecurityControlRequest["phase"]): Promise<Record<string, string>> {
    if (!Number.isInteger(checkpoint) || checkpoint < 0 || pending) throw new Error("Invalid or overlapping security workflow checkpoint");
    pending = true;
    const request: SecurityControlRequest = { id: randomUUID(), scenarioId, checkpoint, phase, expiresAt: Date.now() + 60_000 };
    try {
      const temporary = `${paths.request}.${request.id}.tmp`;
      await writeFile(temporary, `${JSON.stringify(request)}\n`, { mode: 0o600, flag: "wx" });
      await rename(temporary, paths.request);
      while (Date.now() < request.expiresAt) {
        if (await Bun.file(paths.response).exists()) {
          const response = objectValue(await Bun.file(paths.response).json() as unknown, "Actual child security control response");
          if (response.id === request.id) {
            if (response.scenarioId !== scenarioId || response.checkpoint !== checkpoint || response.phase !== phase) throw new Error("Mismatched actual child security control response");
            if (typeof response.error === "string") throw new Error(response.error);
            const observed = objectValue(response.bindings, "Actual child security workflow bindings");
            for (const [token, value] of Object.entries(observed)) {
              if (!hook.bindings.some(binding => binding.token === token)) throw new Error(`Unexpected actual security binding ${token}`);
              if (typeof value !== "string" || value.length === 0) throw new Error(`Invalid actual security binding ${token}`);
              if (Object.hasOwn(bindings, token) && bindings[token] !== value) throw new Error(`Actual security binding changed: ${token}`);
              bindings[token] = value;
            }
            for (const binding of hook.bindings.filter(binding => phase === "after-input" ? binding.checkpoint <= checkpoint : binding.checkpoint < checkpoint)) {
              if (!Object.hasOwn(bindings, binding.token)) throw new Error(`Missing actual child security binding ${binding.token}`);
            }
            if ((bindings.PARITY_SECURITY_AFTER_SCAN_ID !== undefined && bindings.PARITY_SECURITY_AFTER_SCAN_ID === bindings.PARITY_SECURITY_SCAN_ID) || (bindings.PARITY_SECURITY_AFTER_OPERATION_ID !== undefined && bindings.PARITY_SECURITY_AFTER_OPERATION_ID === bindings.PARITY_SECURITY_OPERATION_ID)) throw new Error("Child returned stale original security IDs for the after scan");
            if (phase === "after-input") {
              const bindingTemporary = `${paths.bindings}.${request.id}.tmp`;
              await writeFile(bindingTemporary, `${JSON.stringify(bindings)}\n`, { mode: 0o600, flag: "wx" });
              await rename(bindingTemporary, paths.bindings);
            }
            return { ...bindings };
          }
        }
        await Bun.sleep(25);
      }
      throw new Error(`Timed out awaiting actual child security ${phase} at checkpoint ${checkpoint}`);
    } finally { pending = false; }
  }
  return {
    async beforeInput(checkpoint) { await exchange(checkpoint, "before-input"); },
    async afterInput(checkpoint) { return exchange(checkpoint, "after-input"); },
    requiredHeldProviderTurns(checkpoint) { return hook.heldBeforeObservation.filter(gate => gate.checkpoint === checkpoint).map(gate => gate.providerTurn); },
  };
}
