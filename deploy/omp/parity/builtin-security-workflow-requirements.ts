import type { BuiltinProviderTurn, BuiltinRequirements } from "./builtin-requirements";

export type SecurityWorkflowToken =
  | "PARITY_SECURITY_PLAN_ID"
  | "PARITY_SECURITY_SCAN_ID"
  | "PARITY_SECURITY_OPERATION_ID"
  | "PARITY_SECURITY_FINDING_ID"
  | "PARITY_SECURITY_AFTER_SCAN_ID"
  | "PARITY_SECURITY_AFTER_OPERATION_ID";

export interface SecurityWorkflowBinding {
  checkpoint: number;
  token: SecurityWorkflowToken;
  source: "latest-plan-id" | "latest-operation-scan-id" | "latest-operation-id" | "first-scan-first-finding-id";
}

export interface SecurityWorkflowHook {
  modelRegistry: {
    provider: "openai-codex";
    api: "openai-completions";
    ids: ["parity-coder", "parity-small"];
    baseUrl: "owned-loopback-provider";
    discovery: "disabled";
    runtimeApiKeyOverride: "absent";
  };
  oauthSeed: {
    provider: "openai-codex";
    access: "parity-local-security-access";
    refresh: "parity-local-security-refresh";
    accountId: "parity-security-account";
    expiresAfterMilliseconds: 3600000;
    method: "AuthStorage.credentials.set";
  };
  bindings: SecurityWorkflowBinding[];
  bindingsFileEnvironment: "OMP_PARITY_SECURITY_BINDINGS";
  waitBeforeInput: Array<{ checkpoint: number; operationToken: SecurityWorkflowToken }>;
  waitBeforeObservation: Array<{ checkpoint: number; operationToken: SecurityWorkflowToken }>;
  heldBeforeObservation: Array<{ checkpoint: number; providerTurn: number }>;
  interpolation: "replace-bound-tokens-in-input-observations-and-provider-turn-on-request";
  capture: string;
}

const capture = [
  "Extend builtin.security with workflow, preserving other security captures. Resolve the actual coordinator with getSecurityCoordinator using the real session host (cwd/settings/authStorage/modelRegistry/activeModel/sessionId/agentId/asyncJobManager), never new SecurityCoordinator with injected dependencies.",
  "Every observation reopens SecurityStore.openForCwd(cwd). workflow.plan = getPlan(bound planId); workflow.operation = coordinator.status(bound operationId); workflow.bundle = getBundle(bound scanId); workflow.afterBundle = getBundle(bound afterScanId) when bound; workflow.scanCount = listScans().length. Never select the after-scan as the original scan.",
  "workflow.disk.plan is parsed from <store.projectDirectory>/plans/<planId>.json. workflow.disk.bundle and afterBundle are built only from actual scans/<id>/scan.json, findings.json, optional report.md and results.sarif. planMatchesStore and afterBundleMatchesStore compare the parsed files with the corresponding store read, not with fixture literals.",
  "workflow.disk.sarifMatchesCanonical compares the actual scans/<scanId>/results.sarif JSON with exportSecurityBundleToSarif(await reopenedStore.getBundle(scanId)); do not regenerate or repair stored files in the observer.",
  "workflow.output.scan/findings/sarif/report are actual parsed scan.json/findings.json/results.sarif and report.md under workflow.plan.output.root. workflow.scanSessionFileExists is Bun.file(actual operation.sessionFile).exists(), not a callback invocation count. Keep absent artifacts absent.",
  "Observe workspace exports through existing builtin.workspace-file. Session, plan, store, output and export files all live inside the owned run/HOME boundary. No writes from observers, no SecurityStore seeding for these workflows, no replacement coordinator/session/tools, no imported SARIF shortcut.",
].join(" ");

function nativeHook(): SecurityWorkflowHook {
  return {
    modelRegistry: { provider: "openai-codex", api: "openai-completions", ids: ["parity-coder", "parity-small"], baseUrl: "owned-loopback-provider", discovery: "disabled", runtimeApiKeyOverride: "absent" },
    oauthSeed: { provider: "openai-codex", access: "parity-local-security-access", refresh: "parity-local-security-refresh", accountId: "parity-security-account", expiresAfterMilliseconds: 3600000, method: "AuthStorage.credentials.set" },
    bindings: [
      { checkpoint: 0, token: "PARITY_SECURITY_PLAN_ID", source: "latest-plan-id" },
      { checkpoint: 1, token: "PARITY_SECURITY_SCAN_ID", source: "latest-operation-scan-id" },
      { checkpoint: 1, token: "PARITY_SECURITY_OPERATION_ID", source: "latest-operation-id" },
    ],
    bindingsFileEnvironment: "OMP_PARITY_SECURITY_BINDINGS",
    waitBeforeInput: [], waitBeforeObservation: [], heldBeforeObservation: [],
    interpolation: "replace-bound-tokens-in-input-observations-and-provider-turn-on-request", capture,
  };
}

const lifecycle = nativeHook();
lifecycle.bindings.push(
  { checkpoint: 2, token: "PARITY_SECURITY_FINDING_ID", source: "first-scan-first-finding-id" },
  { checkpoint: 17, token: "PARITY_SECURITY_AFTER_SCAN_ID", source: "latest-operation-scan-id" },
  { checkpoint: 17, token: "PARITY_SECURITY_AFTER_OPERATION_ID", source: "latest-operation-id" },
);
lifecycle.heldBeforeObservation = [{ checkpoint: 1, providerTurn: 0 }, { checkpoint: 17, providerTurn: 8 }];
lifecycle.waitBeforeInput = [{ checkpoint: 2, operationToken: "PARITY_SECURITY_OPERATION_ID" }, { checkpoint: 18, operationToken: "PARITY_SECURITY_AFTER_OPERATION_ID" }];
const cancel = nativeHook();
cancel.heldBeforeObservation = [{ checkpoint: 1, providerTurn: 0 }];
cancel.waitBeforeObservation = [{ checkpoint: 2, operationToken: "PARITY_SECURITY_OPERATION_ID" }];
const unpublished = nativeHook();
unpublished.waitBeforeObservation = [{ checkpoint: 1, operationToken: "PARITY_SECURITY_OPERATION_ID" }];

/** Runner boundary contract; checkpoint indices are zero-based after commandCase expansion. */
export const securityWorkflowHooks: Readonly<Record<string, SecurityWorkflowHook>> = {
  "builtin-security-native-lifecycle": lifecycle,
  "builtin-security-native-cancel": cancel,
  "builtin-security-native-unpublished": unpublished,
};

const reviewRequest = "Run the immutable security plan below.";
const readSource = { id: "parity-security-read", name: "read", arguments: { path: "parity-vulnerable.ts" } };
const finding = {
  rule_id: "PARITY-EVAL", title: "Parity executable input reaches eval",
  summary: "The fixture passes its input directly to eval.", severity: "high", confidence: "high", category: "code-injection", cwe: ["CWE-95"],
  locations: [{ path: "parity-vulnerable.ts", start_line: 1, role: "sink" }],
  evidence: [{ label: "Parity eval sink", explanation: "The input parameter is passed to eval on line 1.", excerpt: "eval(input)", location: { path: "parity-vulnerable.ts", start_line: 1 } }],
  remediation: "Do not evaluate untrusted input.", validation: "unvalidated",
};

const turns: Record<string, BuiltinProviderTurn[]> = {
  "builtin-security-native-lifecycle": [
    { match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_REVIEW_PENDING", toolCalls: [readSource], holdAfterText: true, chunkDelayMs: 0 },
    { match: { textIncludes: reviewRequest }, toolCalls: [{ id: "parity-security-publish", name: "security_publish", arguments: { findings: [finding], coverage: { completeness: "complete", surfaces: [{ label: "Parity eval entrypoint", disposition: "reported", risk_area: "code-injection" }] }, report: "Parity native security report\n" } }], chunkDelayMs: 0 },
    { match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_PUBLISHED" },
    { match: { textIncludes: "Validate security finding" }, toolCalls: [{ ...readSource, id: "parity-security-validation-read" }], chunkDelayMs: 0 },
    { match: { textIncludes: "Validate security finding" }, toolCalls: [{ id: "parity-security-validate", name: "security_scan", arguments: { action: "validate", scan_id: "PARITY_SECURITY_SCAN_ID", finding_id: "PARITY_SECURITY_FINDING_ID", validation_status: "validated", validation_summary: "The committed fixture forwards its input to eval without a guard.", validation_evidence: [{ label: "Parity source validation", explanation: "The real read tool returned line 1 with eval(input)." }] } }], chunkDelayMs: 0 },
    { match: { textIncludes: "Validate security finding" }, text: "PARITY_SECURITY_VALIDATED" },
    { match: { textIncludes: "Validate security finding" }, toolCalls: [{ id: "parity-security-reject", name: "security_scan", arguments: { action: "validate", scan_id: "PARITY_SECURITY_SCAN_ID", finding_id: "PARITY_SECURITY_FINDING_ID", validation_status: "rejected", validation_summary: "The isolated fixture is intentionally executable test data, not a deployed entrypoint.", validation_evidence: [{ label: "Parity fixture limitation", explanation: "This repository is owned deterministic parity data." }] } }], chunkDelayMs: 0 },
    { match: { textIncludes: "Validate security finding" }, text: "PARITY_SECURITY_REJECTED" },
    { match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_SECOND_REVIEW_PENDING", toolCalls: [{ ...readSource, id: "parity-security-second-read" }], holdAfterText: true, chunkDelayMs: 0 },
    { match: { textIncludes: reviewRequest }, toolCalls: [{ id: "parity-security-second-publish", name: "security_publish", arguments: { findings: [], coverage: { completeness: "complete", surfaces: [{ label: "Parity isolated fixture", disposition: "not_applicable", notes: "Intentional executable test data." }] }, report: "Parity isolated fixture review: no deployed entrypoint.\n" } }], chunkDelayMs: 0 },
    { match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_SECOND_PUBLISHED" },
  ],
  "builtin-security-native-cancel": [{ match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_CANCEL_PENDING", holdAfterText: true, chunkDelayMs: 0 }],
  "builtin-security-native-unpublished": [{ match: { textIncludes: reviewRequest }, text: "PARITY_SECURITY_REVIEW_INCOMPLETE" }],
};

export function applySecurityWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const [id, hook] of Object.entries(securityWorkflowHooks)) {
    const requirement = requirements[id];
    if (!requirement) throw new Error(`Missing security workflow case ${id}`);
    const providerTurns = turns[id];
    if (!providerTurns) throw new Error(`Missing security provider tape ${id}`);
    requirement.git = {
      branch: "parity-main", worktreeRoot: "owned-run-directory", dirtyFiles: {},
      commitFiles: {
        "parity-vulnerable.ts": "export function execute(input: string) { return eval(input); }\n",
        "parity-excluded/ignored.ts": "export const ignored = true;\n",
        "parity-security-kb.md": "Parity repository is isolated executable test data. Review the scoped source; never execute it.\n",
      },
    };
    requirement.models.provider = hook.modelRegistry.provider;
    requirement.models.turns = providerTurns;
    requirement.models.roles = { default: "openai-codex/parity-coder", smol: "openai-codex/parity-small" };
    requirement.prerequisites.push(
      "REQUIRED securityWorkflowHooks integration: do not execute these cases with the default plain-openai registry. security/auth.ts rejects API-key-only openai auth; seed one fresh owned openai-codex OAuth row through AuthStorage.credentials.set before ModelRegistry creation. Read its actual row ID; never hardcode it, register a fake OAuth provider, borrow operator auth, or override AuthStorage methods. No runtime/config API-key override may suppress the row. Its unexpired local-only token is accepted only at the owned completion fixture.",
      "Use the real custom model configuration path to define openai-codex/parity-coder and parity-small with API openai-completions and the owned provider baseUrl; no model discovery. Native scan createDefaultSecuritySession must run unchanged, including createSecurityAuthResolver and actual read/security_publish tools. Fresh single OAuth row and custom parity model have no account-ranking/plan-gate usage requirement; all non-loopback requests remain denied and counted, never ignored.",
      "Interpret securityWorkflowHooks for this case before running. Initial command dispatch precedes checkpoint 0. Bind once at indicated checkpoint after command settlement, before observations; latest-plan-id reads listPlans()[0].id and latest-operation-* reads listOperations()[0] fields from the actual coordinator. first-scan-first-finding-id reads getBundle(bound original scanId).findings[0].id. Preserve each binding independently per adapter; fail if absent. Binding is observation, not UUID replacement.",
      "Substitute already-bound tokens in terminal input/browser fill before dispatch; in expected strings, selectors, patterns and names before observation; and in provider tool arguments at request time. Tokens are regex-safe uppercase identifiers. Never substitute an unbound token, mutate a source handler, manufacture an operation ID, or share actual IDs between adapters.",
      "Before checkpoint input: release its releaseProviderTurns, then execute waitBeforeInput using the actual coordinator.wait(bound operationId). After input settlement: acquire new bindings, waitBeforeObservation, then heldBeforeObservation (actual provider held stream map contains requested turn and coordinator is reviewing), and finally capture observations. Held gates must not finish or replace a stream. Cancel is the editor /security cancel operation, not provider.stop(). Any hook absent is a hard unmet prerequisite, never a skipped scenario.",
      "Reopen store and read actual saved files on each checkpoint using hook.capture. Persisted session storage/resume uses the existing actual lifecycle; no pre-seeded plan/bundle/findings are permitted. Compare uses two separately native-published scans; the no-findings second scan represents adjudication of intentional fixture data, not a claimed source-code fix.",
    );
    for (const observation of requirement.captures) if (observation.channel === "builtin.security") observation.source = hook.capture;
  }
}
