import { isDeepStrictEqual } from "node:util";
import { join } from "node:path";
import type { SecurityStore } from "@omp-source/packages/coding-agent/src/security/store";
import type { CaptureState } from "./builtin-capture-state";
import { exists, objectValue, ownedPath, readOwnedText, requestedPointers } from "./builtin-capture-state";
import { actualSecurityCoordinator } from "./builtin-capture-security-control";

async function readDirectoryBundle(state: CaptureState, directory: string): Promise<Record<string, unknown>> {
  const bundle: Record<string, unknown> = {
    scan: JSON.parse(await readOwnedText(state, join(directory, "scan.json"))) as unknown,
    findings: JSON.parse(await readOwnedText(state, join(directory, "findings.json"))) as unknown,
  };
  for (const [file, field, json] of [["report.md", "report", false], ["results.sarif", "sarif", true]] as const) {
    const path = join(directory, file);
    if (await exists(state, path)) {
      const content = await readOwnedText(state, path);
      bundle[field] = json ? JSON.parse(content) as unknown : content;
    }
  }
  return bundle;
}

export async function captureSecurityWorkflow(state: CaptureState, store: SecurityStore): Promise<unknown> {
  const bindingPath = process.env.OMP_PARITY_SECURITY_BINDINGS;
  if (!bindingPath) throw new Error("Security workflow capture requires actual per-leg OMP_PARITY_SECURITY_BINDINGS");
  const bindings = objectValue(JSON.parse(await readOwnedText(state, bindingPath)) as unknown, "Actual security workflow bindings");
  const planId = bindings.PARITY_SECURITY_PLAN_ID;
  if (typeof planId !== "string") throw new Error("Missing actual bound security plan id");
  const plan = await store.getPlan(planId);
  if (!plan) throw new Error(`Bound actual security plan does not exist: ${planId}`);
  const coordinator = await actualSecurityCoordinator(state);
  const diskPlan = JSON.parse(await readOwnedText(state, join(store.projectDirectory, "plans", `${planId}.json`))) as unknown;
  const disk: Record<string, unknown> = { plan: diskPlan, planMatchesStore: isDeepStrictEqual(diskPlan, plan) };
  const workflow: Record<string, unknown> = { plan, disk, scanCount: (await store.listScans()).length };
  const operationId = bindings.PARITY_SECURITY_OPERATION_ID;
  if (typeof operationId === "string") {
    const operation = await coordinator.status(operationId);
    if (!operation) throw new Error(`Bound actual security operation does not exist: ${operationId}`);
    workflow.operation = operation;
    if (operation.sessionFile) workflow.scanSessionFileExists = await exists(state, operation.sessionFile);
  }
  for (const [token, field] of [["PARITY_SECURITY_SCAN_ID", "bundle"], ["PARITY_SECURITY_AFTER_SCAN_ID", "afterBundle"]] as const) {
    const scanId = bindings[token];
    if (scanId === undefined) continue;
    if (typeof scanId !== "string") throw new Error(`Invalid actual security binding: ${token}`);
    const bundle = await store.getBundle(scanId);
    if (!bundle) throw new Error(`Bound actual security scan does not exist: ${scanId}`);
    workflow[field] = bundle;
    const persisted = await readDirectoryBundle(state, join(store.projectDirectory, "scans", scanId));
    disk[field] = persisted;
    if (field === "afterBundle") disk.afterBundleMatchesStore = isDeepStrictEqual(persisted, bundle);
    if (field === "bundle" && persisted.sarif !== undefined) {
      const sarif = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/security/sarif")>("packages/coding-agent/src/security/sarif.ts");
      disk.sarifMatchesCanonical = isDeepStrictEqual(persisted.sarif, sarif.exportSecurityBundleToSarif(bundle));
    }
  }
  if (requestedPointers(state, "builtin.security").some(parts => parts[0] === "workflow" && parts[1] === "output")) {
    const root = await ownedPath(state, plan.output.root);
    workflow.output = await readDirectoryBundle(state, root);
  }
  return workflow;
}
