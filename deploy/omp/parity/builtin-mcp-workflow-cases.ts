import type { BuiltinCase } from "./scenario";
import { commandCase, effect, escape, observed, send } from "./builtin-steps";

const hookField = "[data-omp-role='omp.hook-input.field'] input";
const submitHook = "[data-omp-role='omp.overlay.hook-input'] button:has-text('Submit')";
const selectFirst = [{ type: "press", key: "Enter" }] satisfies NonNullable<BuiltinCase["checkpoints"][number]["browserActions"]>;
const authPresent = () => effect("builtin.mcp", true, "/parity/oauthCredentialPresent");

const mcpComponent = (name: string) => `component:packages/coding-agent/src/modes/controllers/mcp-command-controller.ts#${name}`;
const alsoCovers = (item: BuiltinCase, ...ids: string[]): BuiltinCase => ({ ...item, covers: [...item.covers, ...ids] });

export const mcpWorkflowCases: BuiltinCase[] = [
  alsoCovers(commandCase("mcp-http-oauth-lifecycle", "/mcp reauth parity", 'Reauthorized "parity" (project config)', [
    send("/mcp test parity", 'Successfully connected to "parity"', [authPresent(), effect("builtin.mcp", ["echo"], "/parity/tools")]),
    send("/force mcp__parity_echo Echo PARITY_MCP_ECHO through the authenticated owned MCP server.", "PARITY_MCP_ECHO_COMPLETE", [effect("builtin.provider", "PARITY_MCP_ECHO", "/toolResults/0/content"), effect("builtin.mcp", "connected", "/parity/status")]),
    send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), authPresent(), effect("builtin.mcp", "http", "/parity/config/type")]),
    send("/mcp reauth parity", 'Reauthorized "parity" (project config)', [authPresent()]),
    send("/mcp unauth parity", 'Cleared auth for "parity" (project config)', [effect("builtin.mcp", false, "/parity/oauthCredentialPresent")]),
    send("/mcp remove parity --scope project", 'Removed server "parity" from project config', [effect("builtin.mcp", false, "/parity/config/exists")]),
  ], [authPresent(), effect("builtin.mcp", "connected", "/parity/status"), effect("builtin.mcp", "http", "/parity/config/type")]), mcpComponent("McpConnectingBlock"), mcpComponent("MutableHintBlock"), mcpComponent("MCPAuthorizationLinkPrompt")),
  alsoCovers(commandCase("mcp-http-oauth-cancel", "/mcp reauth parity", "Open authorization URL:", [
    { ...observed('Reauthorization cancelled for "parity"', [effect("builtin.mcp", false, "/parity/oauthCredentialPresent")]), terminalInput: "\u001b", browserActions: escape },
    send("/mcp remove parity --scope project", 'Removed server "parity" from project config', [effect("builtin.mcp", false, "/parity/config/exists")]),
  ], [effect("builtin.mcp", false, "/parity/oauthCredentialPresent")]), mcpComponent("MCPAuthorizationLinkPrompt")),
  commandCase("mcp-smithery-auth-deploy-lifecycle", "/mcp smithery-login", "Smithery API key saved.", [
    send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), effect("builtin.smithery", true, "/hasApiKey")]),
    send("/mcp smithery-search parity --scope project --limit 1", 'Registry results for "parity"', [effect("builtin.mcp", false, "/parity-echo/config/exists")]),
    { ...observed("Server name for deploy (default: parity-echo)"), terminalInput: "\r", browserActions: selectFirst },
    { ...observed('Added server "parity-echo" to project config', [effect("builtin.mcp", true, "/parity-echo/config/exists"), effect("builtin.mcp", "connected", "/parity-echo/status"), effect("builtin.mcp", ["echo"], "/parity-echo/tools")]), terminalInput: "parity-echo\r", browserActions: [{ type: "fill", selector: hookField, value: "parity-echo" }, { type: "click", selector: submitHook }] },
    send("/mcp test parity-echo", 'Successfully connected to "parity-echo"'),
    send("/restart", "Parity session", [effect("builtin.process", 2, "/restartCount"), effect("builtin.smithery", true, "/hasApiKey"), effect("builtin.mcp", true, "/parity-echo/config/exists"), effect("builtin.mcp", "connected", "/parity-echo/status")]),
    send("/mcp smithery-search parity --scope user --limit 1 --semantic", 'Registry results for "parity"'),
    { ...observed("MCP Smithery selection cancelled.", [effect("builtin.mcp", true, "/parity-echo/config/exists")]), terminalInput: "\u001b", browserActions: escape },
    send("/mcp smithery-search parity --scope project --limit 1", 'Registry results for "parity"'),
    { ...observed("Server name for deploy (default: parity-echo-2)"), terminalInput: "\r", browserActions: selectFirst },
    { ...observed("MCP deploy cancelled.", [effect("builtin.mcp", false, "/parity-echo-2/config/exists")]), terminalInput: "\u001b", browserActions: escape },
    send("/mcp smithery-logout", "Smithery API key removed.", [effect("builtin.smithery", false, "/hasApiKey")]),
    send("/mcp remove parity-echo --scope project", 'Removed server "parity-echo" from project config', [effect("builtin.mcp", false, "/parity-echo/config/exists")]),
  ], [effect("builtin.smithery", true, "/hasApiKey")]),
  commandCase("mcp-smithery-manual-key-fallback", "/mcp smithery-login", "Smithery API key", [
    { ...observed("Smithery API key saved.", [effect("builtin.smithery", true, "/hasApiKey")]), terminalInput: "parity-owned-key\r", browserActions: [{ type: "fill", selector: hookField, value: "parity-owned-key" }, { type: "click", selector: submitHook }] },
    send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), effect("builtin.smithery", true, "/hasApiKey")]),
    send("/mcp smithery-logout", "Smithery API key removed.", [effect("builtin.smithery", false, "/hasApiKey")]),
  ], [effect("builtin.smithery", false, "/hasApiKey")]),
];
