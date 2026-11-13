import type { BuiltinCase } from "./scenario";
import { commandCase, dismissThenSend, effect, send } from "./builtin-steps";

export const serviceCases: BuiltinCase[] = [
  commandCase("ssh-local", "/ssh add parity --host 127.0.0.1 --port 2222 --user fixture --desc parity-fixture --compat --scope project", 'Added SSH host "parity" to project config', [
    send("/ssh list", "127.0.0.1", [effect("builtin.ssh", { host: "127.0.0.1", username: "fixture", port: 2222, description: "parity-fixture", compat: true }, "/project/hosts/parity")]),
    dismissThenSend("/ssh remove parity --scope project", 'Removed SSH host "parity" from project config', [effect("builtin.ssh", false, "/project/hosts/parity/exists")]),
    send("/ssh add parity --host 127.0.0.1 --port 2222 --user fixture --scope project", 'Added SSH host "parity" to project config', [effect("builtin.ssh", "127.0.0.1", "/project/hosts/parity/host")]),
    send("/ssh rm parity --scope project", 'Removed SSH host "parity" from project config', [effect("builtin.ssh", false, "/project/hosts/parity/exists")]),
    send("/ssh help", "SSH Host Management"),
    send("/ssh add parity --host 127.0.0.1 --port invalid", "Invalid --port value. Must be an integer between 1 and 65535."),
    send("/ssh add parity --invalid", "Unknown option: --invalid"),
  ]),
  commandCase("mcp-local", "/mcp add parity --scope project -- parity-mcp", 'Added server "parity" to project config', [
    send("/mcp list", "parity", [effect("builtin.mcp", "connected", "/parity/status")]),
    dismissThenSend("/mcp test parity", 'Successfully connected to "parity"', [effect("builtin.mcp", ["echo"], "/parity/tools")]),
    send("/mcp disable parity", 'Disabled "parity" (project config)', [effect("builtin.mcp", false, "/parity/config/enabled")]),
    send("/mcp test parity", 'Server "parity" is disabled. Run /mcp enable parity first.'),
    send("/mcp enable parity", 'Enabled "parity" (project config)', [effect("builtin.mcp", true, "/parity/config/enabled")]),
    send("/mcp reconnect parity", 'Reconnected to "parity"'),
    send("/mcp reload", "MCP reload complete", [effect("builtin.mcp", ["echo"], "/parity/tools")]),
    send("/mcp resources", "Parity resource"),
    dismissThenSend("/mcp prompts", "Parity prompt"),
    dismissThenSend("/mcp notifications", "resources/subscribe"),
    dismissThenSend("/mcp unauth parity", 'No stored OAuth auth to remove for "parity".'),
    send("/mcp remove parity --scope project", 'Removed server "parity" from project config', [effect("builtin.mcp", false, "/parity/config/exists")]),
    send("/mcp add parity --scope project -- parity-mcp", 'Added server "parity" to project config'),
    send("/mcp rm parity --scope project", 'Removed server "parity" from project config'),
  ]),
  ...["remove", "test", "reauth", "unauth", "enable", "disable", "reconnect"].map(verb => commandCase(`mcp-${verb}-missing`, `/mcp ${verb}`, `Server name required. Usage: /mcp ${verb} <name>${verb === "remove" ? " [--scope project|user]" : ""}`)),
  commandCase("mcp-smithery-empty", "/mcp smithery-search", "Keyword required. Usage: /mcp smithery-search <keyword> [--scope project|user] [--limit <1-100>] [--semantic]"),
  commandCase("mcp-help", "/mcp help", "/mcp resources"),
  commandCase("mcp-smithery-logout", "/mcp smithery-logout", "Smithery API key removed.", [], [effect("builtin.smithery", false, "/hasApiKey")]),
  commandCase("mcp-smithery-login-cancel", "/mcp smithery-login", "Smithery API key", [dismissThenSend("/retry", "Nothing to retry", [effect("builtin.smithery", false, "/hasApiKey")])]),
];
