import type { BuiltinCase } from "./scenario";
import { commandCase, effect, send } from "./builtin-steps";

const securityErrors: ReadonlyArray<[string, string, string]> = [
  ["plan", "--invalid", "Unknown security plan option: --invalid"],
  ["scan", "--invalid", "Unknown security plan option: --invalid"],
  ["status", "parity-missing", "Unknown security operation: parity-missing"],
  ["show", "", "show requires a scan id or security:// URI"],
  ["import", "", "import requires a SARIF file or Codex Security bundle directory"],
  ["export", "", "export requires <scan-id> --output <path> [--format bundle|sarif|report]"],
  ["validate", "", "validate requires a finding URI or <scan-id> <finding-id>"],
  ["compare", "", "compare requires <before-scan-id> <after-scan-id>"],
  ["disposition", "", "disposition requires <scan-id> <finding-id> <status> [rationale]"],
];

export const securityCases: BuiltinCase[] = [
  commandCase("security-disabled", "/security", "Security is disabled. Enable security.enabled before using /security."),
  ...securityErrors.map(([verb, args, error]) => commandCase(`security-${verb}-error`, `/security ${verb} ${args}`.trim(), `Security: ${error}`)),
  commandCase("security-empty", "/security scans", "No security scans are stored for this project.", [send("/security cancel parity-missing", "No cancellable security operation parity-missing."), send("/security cancel", "Security: cancel requires an operation id"), send("/security invalid", "Usage: /security <plan|scan|status|cancel|scans|cloud|show|import|export|validate|compare|disposition>")]),
  commandCase("security-import", "/security import parity.sarif", "Imported 1 finding(s) as security scan", [], [effect("builtin.security", 1, "/importedFindingCount"), effect("builtin.security", "Parity import", "/importedTitle")]),
  commandCase("security-disposition", "/security disposition secscan_parity finding_parity accepted_risk parity rationale", "Finding finding_parity disposition is now accepted_risk.", [
    send("/security disposition secscan_parity finding_parity open", "Finding finding_parity disposition is now open.", [effect("builtin.security", "open", "/finding_parity/disposition/status")]),
    send("/security disposition secscan_parity finding_parity fixed", "Security: fixed requires a rationale"),
    send("/security disposition secscan_parity finding_parity wrong", "Security: Unknown disposition: wrong"),
    send("/security export secscan_parity --output security-bundle.json --format bundle", "Exported security scan secscan_parity to", [effect("builtin.workspace-file", "secscan_parity", "/security-bundle.json/json/scan/id")]),
    send("/security export secscan_parity --output security-report.md --format report", "Exported security scan secscan_parity to", [effect("builtin.workspace-file", "Parity report\n", "/security-report.md/content")]),
    send("/security export secscan_parity --output security.sarif --format sarif", "Exported security scan secscan_parity to", [effect("builtin.workspace-file", "2.1.0", "/security.sarif/json/version")]),
    send("/security show secscan_parity", "Parity report"),
    send("/security compare secscan_parity secscan_parity", "unchanged"),
  ], [effect("builtin.security", "accepted_risk", "/finding_parity/disposition/status")]),
  ...["scans", "status", "pull"].map(verb => commandCase(`security-cloud-${verb}`, `/security cloud ${verb} --invalid`, "Security: Unknown security cloud option: --invalid")),
];

for (const scenario of securityCases) scenario.settings = { "security.enabled": scenario.id !== "builtin-security-disabled" };
const imported = securityCases.find(scenario => scenario.id === "builtin-security-import");
if (!imported) throw new Error("Missing explicit security import scenario");
imported.files = { "parity.sarif": JSON.stringify({ version: "2.1.0", runs: [{ tool: { driver: { name: "parity", rules: [{ id: "PARITY001", shortDescription: { text: "Parity import" } }] } }, results: [{ ruleId: "PARITY001", message: { text: "Parity import" }, level: "warning", locations: [{ physicalLocation: { artifactLocation: { uri: "parity.txt" }, region: { startLine: 1 } } }] }] }] }) };
