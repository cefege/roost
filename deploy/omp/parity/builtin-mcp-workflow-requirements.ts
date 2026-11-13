import type { BuiltinRequirements } from "./builtin-requirements";

export function applyMcpWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  const oauth = requirements["builtin-mcp-http-oauth-lifecycle"];
  const cancel = requirements["builtin-mcp-http-oauth-cancel"];
  const smithery = requirements["builtin-mcp-smithery-auth-deploy-lifecycle"];
  const manual = requirements["builtin-mcp-smithery-manual-key-fallback"];
  if (!oauth || !cancel || !smithery || !manual) throw new Error("Missing actual MCP workflow cases");
  for (const [requirement, authorization] of [[oauth, "complete"], [cancel, "hold"]] as const) {
    requirement.services = [{ kind: "mcp-http", behavior: `real Streamable HTTP MCP with canonical dynamic registration and PKCE; authorization ${authorization}`, input: { name: "parity", version: "1.0.0", tool: "echo", resource: "parity://resource", prompt: "parity", oauth: true, authorization } }];
    requirement.prerequisites.push(
      "Seed only actual project MCP config via canonical addMCPServer using the service's real HTTP config and oauth.callbackPort=0. Reauth, token storage, credential lookup, reload and connection remain the unchanged source controller/manager/AuthStorage. Never preseed an access token or replace an OAuth driver.",
      "Follow only the actual source onAuth host-open request for this declared owned service, preserving its PKCE, state and dynamically registered real callback origin. Use reserved public authorization URL mapped to the owned external service at the test transport boundary; do not bypass hostedOpenTarget or synthesize host events. Hold mode returns an actual HTTP waiting page without redirect/token until real Escape cancels canonical flow.",
    );
  }
  oauth.models.turns = [
    { match: { modelId: "parity-coder", textIncludes: "Echo PARITY_MCP_ECHO through the authenticated owned MCP server.", purpose: "foreground" }, toolCalls: [{ id: "parity-http-mcp-echo", name: "mcp__parity_echo", arguments: { text: "PARITY_MCP_ECHO" } }] },
    { match: { modelId: "parity-coder", textIncludes: "Echo PARITY_MCP_ECHO through the authenticated owned MCP server.", purpose: "foreground" }, text: "PARITY_MCP_ECHO_COMPLETE" },
  ];
  for (const [requirement, browserLogin] of [[smithery, "complete"], [manual, "failure"]] as const) {
    requirement.services = [{ kind: "smithery", behavior: `strict owned real registry/auth/deployment transport; browser login ${browserLogin}`, input: { mode: "success", browserLogin, apiKey: "parity-owned-key", scope: "parity", name: "echo", displayName: "Parity MCP" } }];
    requirement.prerequisites.push(
      "Scrub SMITHERY_API_KEY. The initial owned smithery.json is absent. Real controller creates/polls browser auth and validates the returned key through real searchSmitheryRegistry; manual fallback receives HTTP503 only on auth-session creation and validates actually typed key against the same owned registry. Never seed saved credentials or return fixture inputs from capture.",
      "Map only fixed registry.smithery.ai paths through the declared owned Smithery HTTP transport. Follow the actual source's declared reserved-public approval URL onto its owned /approve resource; polling cannot succeed before that real GET. Registry results point to the service's actual owned HTTP MCP endpoint. Deploy invokes unchanged picker/input/config writer/manager; no synthetic server insertion.",
    );
  }
  for (const requirement of [oauth, smithery, manual]) {
    requirement.lifecycle.superviseRealRestart = true;
    requirement.lifecycle.resumeSameSession = true;
    requirement.prerequisites.push("Real /restart retains actual same owned HOME/session/MCP config and credential files. Do not reseed/erase credentials or server config when supervising the next source process.");
  }
}
