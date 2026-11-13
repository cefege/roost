import { rename, writeFile } from "node:fs/promises";
import type { SecurityWorkflowBinding, SecurityWorkflowToken } from "./builtin-security-workflow-requirements";
import { securityWorkflowHooks } from "./builtin-security-workflow-requirements";
import type { CaptureState } from "./builtin-capture-state";
import { exists, objectValue, ownedPath, readOwnedText } from "./builtin-capture-state";
import type { SecurityControlRequest, SecurityControlResponse } from "./security-workflow";
import { securityWorkflowPaths } from "./security-workflow";
import { probe } from "./probe";

export async function actualSecurityCoordinator(state: CaptureState) {
  const session = probe.requireSession();
  const module = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/security/coordinator")>("packages/coding-agent/src/security/coordinator.ts");
  return module.getSecurityCoordinator({
    cwd: session.sessionManager.getCwd(), settings: session.settings,
    authStorage: session.modelRegistry.authStorage, modelRegistry: session.modelRegistry,
    activeModel: session.model, sessionId: session.sessionId, agentId: session.getAgentId(), asyncJobManager: session.asyncJobManager,
  });
}

export function createSecurityControlPoller(state: CaptureState, scenarioId: string): (() => Promise<void>) | undefined {
  const hook = securityWorkflowHooks[scenarioId];
  if (!hook) return undefined;
  const paths = securityWorkflowPaths(state.home);
  const bindings: Partial<Record<SecurityWorkflowToken, string>> = {};
  let processing = false;
  let lastRequestId: string | undefined;

  async function observeBinding(binding: SecurityWorkflowBinding, request: SecurityControlRequest): Promise<string> {
    const session = probe.requireSession();
    const stores = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/security/store")>("packages/coding-agent/src/security/store.ts");
    const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
    await ownedPath(state, utilities.getSecurityProjectDir("parity-control-ownership"));
    const store = await stores.SecurityStore.openForCwd(await ownedPath(state, session.sessionManager.getCwd()));
    await ownedPath(state, store.projectDirectory);
    const coordinator = await actualSecurityCoordinator(state);
    while (Date.now() < request.expiresAt) {
      let value: string | undefined;
      if (binding.source === "latest-plan-id") value = (await store.listPlans())[0]?.id;
      else if (binding.source === "first-scan-first-finding-id") {
        const scanId = bindings.PARITY_SECURITY_SCAN_ID;
        if (!scanId) throw new Error("Original actual security scan is unbound before finding observation");
        value = (await store.getBundle(scanId))?.findings[0]?.id;
      } else {
        const operations = await coordinator.listOperations();
        const after = binding.token === "PARITY_SECURITY_AFTER_SCAN_ID" || binding.token === "PARITY_SECURITY_AFTER_OPERATION_ID";
        const scanId = after ? bindings.PARITY_SECURITY_AFTER_SCAN_ID : bindings.PARITY_SECURITY_SCAN_ID;
        const operation = scanId ? operations.find(operation => operation.scanId === scanId) : operations[0];
        if (operation) {
          value = binding.source === "latest-operation-scan-id" ? operation.scanId : operation.operationId;
          if (after && (operation.scanId === bindings.PARITY_SECURITY_SCAN_ID || operation.operationId === bindings.PARITY_SECURITY_OPERATION_ID)) value = undefined;
        }
      }
      if (value) {
        const previous = bindings[binding.token];
        if (previous !== undefined && previous !== value) throw new Error(`Actual security binding changed: ${binding.token}`);
        return value;
      }
      await Bun.sleep(25);
    }
    throw new Error(`Missing fresh actual security binding before deadline: ${binding.token}`);
  }

  async function execute(request: SecurityControlRequest): Promise<Record<string, string>> {
    for (const binding of hook.bindings.filter(binding => binding.checkpoint < request.checkpoint)) {
      if (!bindings[binding.token]) throw new Error(`Security checkpoint skipped an earlier actual binding: ${binding.token}`);
    }
    const coordinator = await actualSecurityCoordinator(state);
    const waits = request.phase === "before-input" ? hook.waitBeforeInput : hook.waitBeforeObservation;
    if (request.phase === "after-input") {
      for (const binding of hook.bindings.filter(binding => binding.checkpoint === request.checkpoint)) bindings[binding.token] = await observeBinding(binding, request);
    }
    for (const gate of waits.filter(gate => gate.checkpoint === request.checkpoint)) {
      const operationId = bindings[gate.operationToken];
      if (!operationId) throw new Error(`Unbound actual security operation: ${gate.operationToken}`);
      const timeout = Promise.withResolvers<never>();
      const timer = setTimeout(() => timeout.reject(new Error(`Timed out waiting for actual security operation ${operationId}`)), Math.max(1, request.expiresAt - Date.now()));
      try {
        await Promise.race([
          coordinator.wait(operationId),
          timeout.promise,
        ]);
      } finally { clearTimeout(timer); }
    }
    if (request.phase === "after-input" && hook.heldBeforeObservation.some(gate => gate.checkpoint === request.checkpoint)) {
      const operationId = bindings.PARITY_SECURITY_AFTER_OPERATION_ID ?? bindings.PARITY_SECURITY_OPERATION_ID;
      if (!operationId) throw new Error("Held security gate lacks an actual bound operation");
      for (;;) {
        const operation = await coordinator.status(operationId);
        if (!operation) throw new Error(`Bound actual security operation is absent: ${operationId}`);
        if (operation.phase === "reviewing") break;
        if (["completed", "partial", "cancelled", "failed"].includes(operation.phase) || Date.now() >= request.expiresAt) throw new Error(`Actual held security operation is not reviewing: ${operation.phase}`);
        await Bun.sleep(25);
      }
    }
    const observed: Record<string, string> = {};
    for (const [token, value] of Object.entries(bindings)) if (value !== undefined) observed[token] = value;
    return observed;
  }

  return async function pollSecurityControl(): Promise<void> {
    if (processing || !await exists(state, paths.request)) return;
    const decoded = objectValue(JSON.parse(await readOwnedText(state, paths.request)) as unknown, "Owned security control request");
    if (typeof decoded.id !== "string" || decoded.id === lastRequestId) return;
    processing = true;
    lastRequestId = decoded.id;
    const response: SecurityControlResponse = { id: decoded.id, scenarioId, checkpoint: typeof decoded.checkpoint === "number" ? decoded.checkpoint : -1, phase: decoded.phase === "before-input" ? "before-input" : "after-input" };
    try {
      if (decoded.scenarioId !== scenarioId || typeof decoded.checkpoint !== "number" || !Number.isInteger(decoded.checkpoint) || decoded.checkpoint < 0 || !["before-input", "after-input"].includes(String(decoded.phase)) || typeof decoded.expiresAt !== "number" || !Number.isFinite(decoded.expiresAt) || decoded.expiresAt <= Date.now()) throw new Error("Invalid, expired, or mismatched actual security control request");
      const request: SecurityControlRequest = { id: decoded.id, scenarioId, checkpoint: decoded.checkpoint, phase: decoded.phase as SecurityControlRequest["phase"], expiresAt: decoded.expiresAt };
      response.bindings = await execute(request);
    } catch (error) { response.error = error instanceof Error ? error.message : String(error); }
    try {
      const temporary = await ownedPath(state, `${paths.response}.${decoded.id}.tmp`);
      await writeFile(temporary, `${JSON.stringify(response)}\n`, { mode: 0o600, flag: "wx" });
      await rename(temporary, await ownedPath(state, paths.response));
    } finally { processing = false; }
  };
}
