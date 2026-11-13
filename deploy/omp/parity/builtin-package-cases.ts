import type { BuiltinCase } from "./scenario";
import { commandCase, effect, send } from "./builtin-steps";

export const packageCases: BuiltinCase[] = [
  commandCase("marketplace-empty", "/marketplace installed", "No marketplace plugins installed", [send("/marketplace upgrade", "All marketplace plugins are up to date"), send("/marketplace update", "Updated 0 marketplace(s)"), send("/marketplace help", "Marketplace commands:"), send("/marketplace add", "Usage: /marketplace add <source>"), send("/marketplace remove", "Usage: /marketplace remove <name>")]),
  commandCase("marketplace-local", "/marketplace add ./parity-marketplace", "Added marketplace: parity-market", [
    send("/marketplace list", "parity-market"),
    send("/marketplace discover parity-market", "parity-plugin@1.0.0"),
    send("/marketplace update parity-market", "Updated marketplace: parity-market"),
    send("/marketplace install --scope project parity-plugin@parity-market", "Installed parity-plugin from parity-market", [effect("builtin.plugins", true, "/parity-plugin@parity-market/installed")]),
    send("/marketplace installed", "parity-plugin@parity-market"),
    send("/plugins disable --scope project parity-plugin@parity-market", "Disabled parity-plugin@parity-market", [effect("builtin.plugins", false, "/parity-plugin@parity-market/enabled")]),
    send("/plugin enable --scope project parity-plugin@parity-market", "Enabled parity-plugin@parity-market", [effect("builtin.plugins", true, "/parity-plugin@parity-market/enabled")]),
    send("/plugins list", "parity-plugin@parity-market"),
    send("/plugin list", "parity-plugin@parity-market"),
    send("/marketplace upgrade --scope project parity-plugin@parity-market", "Upgraded parity-plugin@parity-market to 1.0.0"),
    send("/marketplace uninstall --scope project parity-plugin@parity-market", "Uninstalled parity-plugin@parity-market", [effect("builtin.plugins", false, "/parity-plugin@parity-market/installed")]),
    send("/marketplace remove parity-market", "Removed marketplace: parity-market", [effect("builtin.marketplaces", [])]),
    send("/marketplace add ./parity-marketplace", "Added marketplace: parity-market"),
    send("/marketplace rm parity-market", "Removed marketplace: parity-market", [effect("builtin.marketplaces", [])]),
  ]),
  ...["plugins", "plugin"].map(token => commandCase(`${token}-empty`, `/${token} list`, "No plugins installed", [send(`/${token} enable`, "Usage: /plugins enable [--scope user|project] <plugin>"), send(`/${token} disable`, "Usage: /plugins disable [--scope user|project] <plugin>")])),
  commandCase("reload-plugins", "/reload-plugins", "Plugins reloaded.", [], [effect("builtin.plugin-reloads", 1)]),
  commandCase("skills-empty", "/skills installed", "No registry skills installed.", [send("/skills search", "Usage: /skills search <query>"), send("/skills install", "Usage: /skills install <@scope/name[@range]>… [--global]"), send("/skills update --invalid", "Unknown option: --invalid"), send("/skills help", "Skill registry (skills.omp.sh) commands:"), send("/skills invalid", "Unknown /skills subcommand: invalid")]),
  {
    ...commandCase("skills-invoke-provider", "/skills install @parity/echo@1.0.0", "+ @parity/echo@1.0.0", [
      send("/skill:echo run the parity skill", "PARITY_SKILL_RESULT", [effect("builtin.provider", 1, "/requestCount")]),
    ]),
    covers: ["command:skills", "component:packages/coding-agent/src/modes/interactive-mode.ts#InteractiveMode"],
  },
  commandCase("skills-local", "/skills search parity", "@parity/echo 1.0.0", [
    send("/skills install @parity/echo@1.0.0", "+ @parity/echo@1.0.0", [effect("builtin.skills", "1.0.0", "/@parity~1echo/version"), effect("builtin.skills", "Parity skill body\n", "/@parity~1echo/content")]),
    send("/skills installed", "@parity/echo 1.0.0"),
    send("/skills update @parity/echo", "Registry skills are already up to date.", [effect("builtin.skills", "1.0.0", "/@parity~1echo/version")]),
  ]),
];
