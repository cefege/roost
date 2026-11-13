import type { BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, effect, send } from "./builtin-steps";

const plan = "PARITY_SECURITY_PLAN_ID";
const scan = "PARITY_SECURITY_SCAN_ID";
const operation = "PARITY_SECURITY_OPERATION_ID";
const finding = "PARITY_SECURITY_FINDING_ID";
const afterScan = "PARITY_SECURITY_AFTER_SCAN_ID";
const afterOperation = "PARITY_SECURITY_AFTER_OPERATION_ID";
const findingUri = `security://scans/${scan}/findings/${finding}`;

function saved(pointer: string, expected: unknown): Comparison[] {
  return [effect("builtin.security", expected, `/workflow/bundle${pointer}`), effect("builtin.security", expected, `/workflow/disk/bundle${pointer}`)];
}

function dispatch(verb: string, command: string, output: string, comparisons: Comparison[] = []): Checkpoint {
  const checkpoint = send(command, output, comparisons);
  const claim = `subcommand:security ${verb}`;
  for (const comparison of checkpoint.comparisons) comparison.proves = [{ entry: claim, effect: comparison.terminal.type === "effect" ? "host" : "interaction" }];
  return checkpoint;
}

function workflow(id: string, command: string, output: string, initial: Comparison[], steps: Checkpoint[], verbs: string[]): BuiltinCase {
  const scenario = commandCase(id, command, output, steps, initial);
  scenario.settings = { "security.enabled": true, "tools.xdev": false };
  scenario.covers.push(...verbs.map(verb => `subcommand:security ${verb}`));
  for (const comparison of scenario.checkpoints[0]!.comparisons) comparison.proves = [
    { entry: "command:security", effect: comparison.terminal.type === "effect" ? "host" : "interaction" },
    { entry: "subcommand:security plan", effect: comparison.terminal.type === "effect" ? "host" : "interaction" },
  ];
  return scenario;
}

const nativePlan = "/security plan --working-tree --path parity-vulnerable.ts --exclude parity-excluded --knowledge-base parity-security-kb.md";
const planned = `Security plan ${plan} is ready.`;
const planSaved = [
  effect("builtin.security", "omp-security.scan-plan", "/workflow/plan/documentType"),
  effect("builtin.security", "working_tree", "/workflow/plan/target/kind"),
  effect("builtin.security", ["parity-vulnerable.ts"], "/workflow/plan/target/includePaths"),
  effect("builtin.security", ["parity-excluded"], "/workflow/plan/target/excludePaths"),
  effect("builtin.security", "parity-coder", "/workflow/disk/plan/model/modelId"),
  effect("builtin.security", "parity-security-account", "/workflow/disk/plan/account/accountId"),
  effect("builtin.security", true, "/workflow/disk/planMatchesStore"),
  effect("builtin.provider", 0, "/requestCount"),
];

const started = dispatch("scan", `/security scan ${plan}`, `Security scan ${scan} started as ${operation}.`, [
  effect("builtin.security", "reviewing", "/workflow/operation/phase"),
  ...saved("/scan/status", "running"),
  ...saved("/findings", []),
  effect("builtin.provider", 1, "/requestCount"),
]);

const completed = dispatch("status", `/security status ${operation}`, '"phase": "completed"', [
  effect("builtin.security", "completed", "/workflow/operation/phase"),
  effect("builtin.security", 1, "/workflow/operation/findingCount"),
  ...saved("/scan/status", "completed"),
  ...saved("/scan/producer/kind", "omp-native"),
  ...saved("/findings/0/title", "Parity executable input reaches eval"),
  ...saved("/findings/0/validation/status", "unvalidated"),
  ...saved("/findings/0/disposition/status", "open"),
  effect("builtin.security", "completed", "/workflow/output/scan/status"),
  effect("builtin.security", "2.1.0", "/workflow/output/sarif/version"),
  effect("builtin.security", "Parity native security report\n", "/workflow/output/report"),
  effect("builtin.security", true, "/workflow/scanSessionFileExists"),
  effect("builtin.provider", ["read", "security_publish"], "/toolNames"),
  effect("builtin.provider", 3, "/requestCount"),
  effect("builtin.provider", 0, "/remoteRequests"),
]);
completed.releaseProviderTurns = [0];

const dispositionSteps = ([
  ["false_positive", "parity false positive rationale"],
  ["accepted_risk", "parity accepted risk rationale"],
  ["fixed", "parity fixed rationale"],
  ["wont_fix", "parity wont fix rationale"],
  ["open", ""],
] satisfies Array<[string, string]>).map(([status, rationale]) => dispatch("disposition", `/security disposition ${scan} ${finding} ${status} ${rationale}`.trim(), `Finding ${finding} disposition is now ${status}.`, [
  ...saved("/findings/0/disposition/status", status),
  ...saved("/findings/0/disposition/actor", "operator"),
  ...(rationale ? saved("/findings/0/disposition/rationale", rationale) : []),
  effect("builtin.security", true, "/workflow/disk/sarifMatchesCanonical"),
]));

const lifecycle = workflow("security-native-lifecycle", nativePlan, planned, planSaved, [
  started,
  completed,
  dispatch("scans", "/security scans", `${scan} completed 1 finding(s)`, [effect("builtin.security", 1, "/workflow/scanCount")]),
  dispatch("show", `/security show security://scans/${scan}/report`, "Parity native security report", saved("/report", "Parity native security report\n")),
  dispatch("show", `/security show ${findingUri}`, "Parity executable input reaches eval", saved("/findings/0/occurrences/0/locations/0/path", "parity-vulnerable.ts")),
  dispatch("export", `/security export ${scan} --output parity-native-bundle.json --format bundle`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "completed", "/parity-native-bundle.json/json/scan/status"), effect("builtin.workspace-file", "open", "/parity-native-bundle.json/json/findings/0/disposition/status")]),
  dispatch("export", `/security export ${scan} --output parity-native-report.md --format report`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "Parity native security report\n", "/parity-native-report.md/content")]),
  dispatch("export", `/security export ${scan} --output parity-native.sarif --format sarif`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "2.1.0", "/parity-native.sarif/json/version"), effect("builtin.workspace-file", "PARITY-EVAL", "/parity-native.sarif/json/runs/0/results/0/ruleId")]),
  dispatch("validate", `/security validate ${findingUri}`, "PARITY_SECURITY_VALIDATED", [
    ...saved("/findings/0/validation/status", "validated"),
    ...saved("/findings/0/validation/summary", "The committed fixture forwards its input to eval without a guard."),
    ...saved("/findings/0/evidence/1/kind", "validation"),
    ...saved("/findings/0/evidence/1/label", "Parity source validation"),
    effect("builtin.security", true, "/workflow/disk/sarifMatchesCanonical"),
    effect("builtin.provider", ["read", "security_publish", "security_scan"], "/toolNames"),
    effect("builtin.workspace-file", "export function execute(input: string) { return eval(input); }\n", "/parity-vulnerable.ts/content"),
  ]),
  ...dispositionSteps,
  dispatch("export", `/security export ${scan} --output parity-native-validated.json --format bundle`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "validated", "/parity-native-validated.json/json/findings/0/validation/status"), effect("builtin.workspace-file", "open", "/parity-native-validated.json/json/findings/0/disposition/status")]),
  dispatch("validate", `/security validate ${scan} ${finding}`, "PARITY_SECURITY_REJECTED", [...saved("/findings/0/validation/status", "rejected"), ...saved("/findings/0/validation/summary", "The isolated fixture is intentionally executable test data, not a deployed entrypoint.")]),
  dispatch("scan", nativePlan.replace("/security plan", "/security scan"), `Security scan ${afterScan} started as ${afterOperation}.`, [effect("builtin.security", "running", "/workflow/afterBundle/scan/status"), effect("builtin.security", "running", "/workflow/disk/afterBundle/scan/status")]),
  { ...dispatch("status", `/security status ${afterOperation}`, '"phase": "completed"', [effect("builtin.security", "completed", "/workflow/afterBundle/scan/status"), effect("builtin.security", [], "/workflow/disk/afterBundle/findings"), effect("builtin.security", true, "/workflow/disk/afterBundleMatchesStore")]), releaseProviderTurns: [8] },
  dispatch("compare", `/security compare ${scan} ${afterScan}`, '"resolved": 1', [effect("builtin.security", 2, "/workflow/scanCount"), ...saved("/findings/0/validation/status", "rejected"), effect("builtin.provider", 0, "/remoteRequests")]),
  send(`/security cancel ${operation}`, `No cancellable security operation ${operation}.`, saved("/scan/status", "completed")),
  send("/security plan --working-tree --path ../parity-escape", "Security: Security scope path must be repository-relative: ../parity-escape", [...saved("/scan/status", "completed"), effect("builtin.security", 2, "/workflow/scanCount")]),
  send(`/security scan ${plan}`, "Security: Security output directory is not empty; enable archiveExisting or choose another directory", [...saved("/scan/status", "completed"), effect("builtin.security", 2, "/workflow/scanCount")]),
], ["plan", "scan", "status", "scans", "show", "export", "validate", "disposition", "compare"]);

const cancelled = workflow("security-native-cancel", nativePlan, planned, planSaved, [
  started,
  dispatch("cancel", `/security cancel ${operation}`, `Cancellation requested for ${operation}.`, [...saved("/scan/status", "cancelled"), ...saved("/findings", [])]),
  dispatch("status", `/security status ${operation}`, '"phase": "cancelled"', [...saved("/scan/status", "cancelled"), ...saved("/findings", []), effect("builtin.provider", true, "/cancelled"), effect("builtin.provider", 0, "/remoteRequests")]),
  dispatch("scans", "/security scans", `${scan} cancelled 0 finding(s)`, [effect("builtin.security", 1, "/workflow/scanCount")]),
  dispatch("export", `/security export ${scan} --output parity-cancelled.json`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "cancelled", "/parity-cancelled.json/json/scan/status"), effect("builtin.workspace-file", [], "/parity-cancelled.json/json/findings")]),
  send(`/security compare ${scan} ${scan}`, `is cancelled`, saved("/scan/status", "cancelled")),
  send(`/security cancel ${operation}`, `No cancellable security operation ${operation}.`, saved("/scan/status", "cancelled")),
], ["plan", "scan", "cancel", "status", "scans", "export"]);

const unpublished = workflow("security-native-unpublished", nativePlan, planned, planSaved, [
  dispatch("scan", `/security scan ${plan}`, `Security scan ${scan} started as ${operation}.`, [...saved("/scan/status", "partial"), ...saved("/findings", [])]),
  dispatch("status", `/security status ${operation}`, "The scan session ended without publishing a canonical result", [
    effect("builtin.security", "partial", "/workflow/operation/phase"), ...saved("/scan/status", "partial"), ...saved("/findings", []),
    ...saved("/scan/error", "The scan session ended without publishing a canonical result"), effect("builtin.provider", 1, "/requestCount"),
  ]),
  dispatch("scans", "/security scans", `${scan} partial 0 finding(s)`),
  send(`/security export ${scan} --output parity-no-report.md --format report`, `Security: Security scan ${scan} has no report`, [effect("builtin.workspace-file", false, "/parity-no-report.md/exists"), ...saved("/scan/status", "partial")]),
  send(`/security compare ${scan} ${scan}`, "is partial", saved("/scan/status", "partial")),
  dispatch("export", `/security export ${scan} --output parity-partial.json`, `Exported security scan ${scan} to`, [effect("builtin.workspace-file", "partial", "/parity-partial.json/json/scan/status")]),
], ["plan", "scan", "status", "scans", "export"]);

export const securityWorkflowCases: BuiltinCase[] = [lifecycle, cancelled, unpublished];
