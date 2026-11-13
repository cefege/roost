import type { BuiltinCase } from "./scenario";
import { commandCase, completion, dismissThenSend, effect, send } from "./builtin-steps";

export const modeCases: BuiltinCase[] = [
  commandCase("settings-cancel", "/settings", "Appearance", [dismissThenSend("/extended-context status", "Extended context is off.")]),
  ...["setup", "providers"].map(token => commandCase(`${token}-cancel`, `/${token}`, "Providers", [dismissThenSend("/setup invalid", "Usage: /setup [providers]")])),
  commandCase("setup-providers", "/setup providers", "Providers", [dismissThenSend("/setup invalid", "Usage: /setup [providers]")]),
  commandCase("plan-disabled", "/plan", "Plan mode is disabled. Enable it in settings (plan.enabled)."),
  commandCase("plan-review-inactive", "/plan-review", "Plan mode is not active."),
  commandCase("vibe-toggle", "/vibe", "Vibe mode enabled. You direct fast/good worker sessions; toolset is read + optional parent Todo + vibe tools.", [send("/vibe", "Vibe mode disabled.", [effect("builtin.session", "none", "/mode")])]),
  commandCase("goal-empty", "/goal show", "No goal set.", [send("/goal pause", "No active goal to pause."), send("/goal resume", "No paused goal to resume."), send("/goal budget 1", "No active goal."), send("/goal drop", "No goal to drop.")]),
  commandCase("guided-goal-disabled", "/guided-goal parity", "Goal mode is disabled. Enable it in settings (goal.enabled)."),
  commandCase("queue-empty", "/queue", "Usage: /queue <message> (or start a prompt with -> / =>)"),
  ...["model", "models"].map(token => commandCase(`${token}-cancel`, `/${token}`, "Models", [dismissThenSend("/switch parity-missing-model", "Unknown model: parity-missing-model")])),
  commandCase("switch-error", "/switch parity-missing-model", "Unknown model: parity-missing-model"),
  commandCase("fast", "/fast off", "Fast mode disabled.", [send("/fast status", "Fast mode is off."), send("/fast invalid", "Usage: /fast [on|ultra|off|status]"), send("/fast on", "Fast mode enabled."), send("/fast status", "Fast mode is on."), send("/fast off", "Fast mode disabled.")]),
  commandCase("slow", "/slow off", "Slow mode off.", [send("/slow on", "Slow mode on: requests use the flex tier (lower cost, higher latency)."), send("/slow status", "Slow mode is on (flex tier)."), send("/slow invalid", "Usage: /slow [on|off|status]"), send("/slow off", "Slow mode off.")]),
  commandCase("skillful", "/skillful on", "Skill listing enabled for this session.", [send("/skillful status", "Skill listing: on."), send("/skillful off", "Skill listing disabled for this session."), send("/skillful status", "Skill listing: off."), send("/skillful invalid", "Usage: /skillful [on|off|status]")]),
  commandCase("extended-context", "/extended-context on", "Extended context enabled.", [send("/extended-context status", "Extended context is on.", [effect("builtin.persisted-settings", true, "/extendedContext")]), send("/extended-context off", "Extended context disabled."), send("/extended-context status", "Extended context is off."), send("/extended-context invalid", "Usage: /extended-context [on|off|status]")]),
  commandCase("computer", "/computer off", "Computer use disabled for this session.", [send("/computer on", "Computer use is unavailable in this session."), send("/computer invalid", "Usage: /computer [on|off|status]")]),
  commandCase("ratchet-unavailable", "/ratchet parity", "/ratchet needs the eval and task tool active."),
  commandCase("prewalk-off", "/prewalk off", "Prewalk already off for this session; keeping the active model.", [send("/prewalk restart a b c", "Usage: /prewalk [into] | restart [from] [into] | off")]),
  commandCase("preset-empty", "/modelpreset list", "No model presets saved. Use /modelpreset save <name> to create one.", [send("/modelpreset save", "Usage: /modelpreset save <name>"), send("/modelpreset delete absent", "Preset not found: absent"), send("/modelpreset save 1invalid", 'Invalid preset name "1invalid": use a letter, then letters, digits, - or _')]),
  commandCase("effort-set", "/effort low", "Thinking set to low.", [send("/effort high", "Thinking set to high.", [effect("builtin.session", "high", "/thinkingLevel")])]),
  completion("/effort low", "/effort hi", "/effort high ", "Thinking set to high."),
  completion("/extended-context off", "/extended-context sta", "/extended-context status ", "Extended context is off."),
  completion("/prewalk off", "/prewalk of", "/prewalk off ", "Prewalk already off for this session; keeping the active model."),
];

for (const scenario of modeCases) {
  scenario.settings = { extendedContext: false, "plan.enabled": false, "goal.enabled": scenario.id === "builtin-goal-empty", "computer.enabled": false, ...scenario.settings };
}
