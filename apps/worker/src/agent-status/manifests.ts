// Pinned/adapted from Herdr src/detect/manifests/*.toml at commit
// eacea2daf0b72973173b728936b27478374f2cd2 (Apache-2.0).

import type { AgentManifest } from "./manifest-engine.ts";
import type { BuiltinAgentId } from "./process-scan.ts";

const codex: AgentManifest = {
  id: "codex", version: "2026.07.18.1", rules: [
    { id: "osc_title_blocked", state: "blocked", priority: 1100, region: "osc_title", visible_blocker: true, contains: ["Action Required"] },
    { id: "osc_title_working", state: "working", priority: 1050, region: "osc_title", visible_working: true, regex: [String.raw`(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)`] },
    { id: "transcript_viewer", state: "unknown", priority: 1000, region: "after_last_prompt_marker", skip_state_update: true,
      contains: ["↑/↓ to scroll", "pgup/pgdn to", "home/end to jump", "q to quit"],
      any: [{ contains: ["esc to edit prev"] }, { contains: ["esc/← to edit prev"] }] },
    { id: "live_strong_blocker", state: "blocked", priority: 900, region: "after_last_prompt_marker", visible_blocker: true,
      any: ["press enter to confirm or esc to cancel", "enter to submit answer", "enter to submit all", "allow command?"].map((value) => ({ contains: [value] })) },
    { id: "weak_blocker", state: "blocked", priority: 600, any: [
      { contains: ["[y/n]"] }, { contains: ["yes (y)"] },
      { contains: ["do you want to"], any: [{ contains: ["yes"] }, { contains: ["❯"] }] },
      { contains: ["would you like to"], any: [{ contains: ["yes"] }, { contains: ["❯"] }] },
    ] },
    { id: "screen_working_fallback", state: "working", priority: 500, region: "bottom_non_empty_lines(3)", visible_working: true,
      line_regex: [String.raw`^[•◦]\s+Working \([^)]*esc to interrupt\)(?: · .*)?$`], not: [{ contains: ["■ Conversation interrupted"] }] },
    { id: "osc_title_idle", state: "idle", priority: 100, region: "osc_title", visible_idle: true, regex: [String.raw`\S`],
      not: [{ regex: [String.raw`(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)`] }, { contains: ["Action Required"] }] },
  ],
};

const gemini: AgentManifest = {
  id: "gemini", version: "2026.06.10.1", rules: [
    { id: "apply_or_allow_change", state: "blocked", priority: 300, visible_blocker: true, any: [
      { contains: ["│ Apply this change"] }, { contains: ["│ Allow execution"] },
      { all: [{ contains: ["yes"] }, { any: ["waiting for user confirmation", "│ Do you want to proceed", "do you want to proceed?"].map((value) => ({ contains: [value] })) }] },
      { line_regex: [String.raw`(?i)^\s*❯.*(yes|allow)`] },
    ] },
    { id: "esc_cancel_working", state: "working", priority: 100, visible_working: true, contains: ["esc to cancel"] },
  ],
};

const opencode: AgentManifest = {
  id: "opencode", version: "2026.06.10.1", aliases: ["open-code", "herdr:opencode"], rules: [
    { id: "permission_required", state: "blocked", priority: 300, visible_blocker: true, any: [
      { contains: ["△ Permission required"] },
      { contains: ["esc dismiss"], any: ["enter confirm", "enter submit", "enter toggle"].map((value) => ({ contains: [value] })),
        all: [{ any: [{ contains: ["↑↓ select"] }, { contains: ["⇆ tab"] }] }] },
    ] },
    { id: "interrupt_hint_working", state: "working", priority: 110, visible_working: true, any: [
      ...["esc to interrupt", "ctrl+c to interrupt", "press esc to interrupt"].map((value) => ({ contains: [value] })),
      { line_regex: [String.raw`(?i).*opencode.*esc (again to )?interrupt`] },
    ] },
    { id: "progress_bar_working", state: "working", priority: 100, visible_working: true, regex: ["(■|⬝){4,}"] },
  ],
};

const cursor: AgentManifest = {
  id: "cursor", version: "2026.06.10.1", aliases: ["cursor-agent"], rules: [
    { id: "write_file_approval", state: "blocked", priority: 320, region: "bottom_non_empty_lines(8)", visible_blocker: true,
      contains: ["write to this file?", "proceed (y)"], any: ["reject & propose changes", "esc or n or p", "add write("].map((value) => ({ contains: [value] })) },
    { id: "approval_prompt", state: "blocked", priority: 300, visible_blocker: true, any: [
      { contains: ["waiting for approval", "run this command?"], any: [{ contains: ["run (once) (y)"] }, { contains: ["skip (esc or n)"] }] },
      ...["(y) (enter)", "keep (n)", "skip (esc or n)"].map((value) => ({ contains: [value] })),
      { line_regex: [String.raw`(?i)^\s*allow .*\(y\)`] },
      { line_regex: [String.raw`(?i)^\s*(run |.*\(y\).*(allow|run \(once\)|→ run))`] },
    ] },
    { id: "stop_hint_working", state: "working", priority: 100, region: "bottom_non_empty_lines(6)", visible_working: true, contains: ["ctrl+c to stop"] },
    { id: "background_task_status_working", state: "working", priority: 95, region: "bottom_non_empty_lines(5)", visible_working: true,
      line_regex: [String.raw`(?i)\b[1-9][0-9]*\s+background\s+tasks?\b`] },
    { id: "spinner_working", state: "working", priority: 90, region: "bottom_non_empty_lines(8)", visible_working: true,
      line_regex: [String.raw`^\s*(⬡|⬢|[\u2800-\u28FF]+)\s+\p{Alphabetic}+\w*ing\b`] },
  ],
};

const amp: AgentManifest = {
  id: "amp", version: "2026.07.09.1", aliases: ["amp-local"], rules: [
    { id: "osc_title_plugin_confirmation_blocked", state: "blocked", priority: 1100, region: "osc_title", visible_blocker: true, contains: ["Plugin confirmation needed"] },
    { id: "osc_title_working", state: "working", priority: 1050, region: "osc_title", visible_working: true, regex: [String.raw`^[\x{2800}-\x{28FF}] `] },
    { id: "approval_footer", state: "blocked", priority: 300, visible_blocker: true, any: [
      ...["waiting for approval", "invoke tool", "run this command?", "allow editing file:", "allow creating file:", "confirm tool call"].map((value) => ({ contains: [value] })),
      { contains: ["approve"], any: ["allow all for this session", "allow all for every session", "allow file for every session", "deny with feedback"].map((value) => ({ contains: [value] })) },
    ] },
    { id: "status_footer_working", state: "working", priority: 200, region: "bottom_non_empty_lines(5)", visible_working: true,
      line_regex: [String.raw`(?i)^\s*╰\s+\S+\s+(thinking|streaming|running tools|waiting)\s+─`] },
    { id: "esc_cancel_working", state: "working", priority: 100, visible_working: true, contains: ["esc to cancel"] },
    { id: "osc_title_idle", state: "idle", priority: 50, region: "osc_title", visible_idle: true, contains: [" - amp - "],
      not: [{ regex: [String.raw`^[\x{2800}-\x{28FF}] `] }, { contains: ["Plugin confirmation needed"] }] },
  ],
};

const copilot: AgentManifest = {
  id: "copilot", version: "2026.07.07.1", aliases: ["github-copilot", "ghcs"], rules: [
    { id: "selection_blocker", state: "blocked", priority: 300, visible_blocker: true, all: [
      { any: [{ contains: ["esc to cancel"] }, { contains: ["esc cancel"] }] },
      { any: ["enter to select", "enter to confirm", "enter to submit", "enter accept"].map((value) => ({ contains: [value] })) },
    ] },
    { id: "working_cancel_hint", state: "working", priority: 100, visible_working: true,
      any: ["esc to cancel", "esc cancel", "esc again to cancel", "esc interrupt"].map((value) => ({ contains: [value] })) },
  ],
};

const droid: AgentManifest = {
  id: "droid", version: "2026.06.10.1", rules: [
    { id: "execute_selection_blocker", state: "blocked", priority: 300, visible_blocker: true,
      contains: ["enter to select", "esc to cancel"], any: [{ contains: ["↑↓ to navigate"] }, { contains: ["use ↑↓ to navigate"] }],
      all: [{ any: [{ contains: ["> yes, allow"] }, { contains: ["> no, cancel"] }] }] },
    { id: "selection_menu_blocker", state: "blocked", priority: 290, region: "bottom_non_empty_lines(8)", visible_blocker: true,
      contains: ["enter select", "esc cancel"], any: [{ contains: ["↑/↓ navigate"] }, { contains: ["↑↓ navigate"] }] },
    { id: "spinner_stop_working", state: "working", priority: 110, visible_working: true,
      contains: ["esc to stop"], line_regex: [String.raw`^\s*[\u2800-\u28FF]`] },
    { id: "stop_hint_working", state: "working", priority: 100, visible_working: true, contains: ["esc to stop"] },
  ],
};

const grok: AgentManifest = {
  id: "grok", version: "2026.07.16.2", aliases: ["grok-build"], rules: [
    { id: "osc_title_blocked", state: "blocked", priority: 1300, region: "osc_title", visible_blocker: true, contains: ["Action Required"] },
    { id: "option_dialog_blocked", state: "blocked", priority: 1200, visible_blocker: true, line_regex: [String.raw`^\s*┃\s+[0-9a-z]+\s+\([●○]\)\s`] },
    { id: "permission_hints_blocked", state: "blocked", priority: 1190, region: "bottom_non_empty_lines(2)", visible_blocker: true, contains: [":select", "ctrl+o:yolo", "ctrl+c:cancel"] },
    { id: "question_dialog_hints_blocked", state: "blocked", priority: 1185, region: "bottom_non_empty_lines(2)", visible_blocker: true, contains: ["tab:scrollback", "shift+x:dismiss"] },
    { id: "permission_scope_selector", state: "blocked", priority: 1180, visible_blocker: true, contains: ["yes, proceed", "no, reject"],
      any: [{ contains: ["use ← → to choose permission whitelist scope"] }, { contains: ["←/→:scope"] }] },
    { id: "background_work_chip_working", state: "working", priority: 1170, region: "top_non_empty_lines(1)", visible_working: true,
      line_regex: [String.raw`[⋅:⸬⁙.·]\s+[1-9][0-9]*\s+│`] },
    { id: "osc_progress_working", state: "working", priority: 1150, region: "osc_progress", visible_working: true, regex: [String.raw`^4;1;-1$`] },
    { id: "osc_title_idle", state: "idle", priority: 1100, region: "osc_title", visible_idle: true, regex: [String.raw`(?:^| - )grok$`], not: [{ regex: [String.raw`[\x{2800}-\x{28FF}]`] }] },
    { id: "osc_title_working", state: "working", priority: 1000, region: "osc_title", visible_working: true, regex: [String.raw`\S`] },
    { id: "osc_progress_idle", state: "idle", priority: 950, region: "osc_progress", visible_idle: true, regex: [String.raw`^4;0;0$`] },
    { id: "spinner_status_working", state: "working", priority: 200, visible_working: true, line_regex: [String.raw`^\s*[\x{2801}-\x{28FF}]\s.*\[stop\]\s*$`] },
    { id: "esc_cancel_hints_working", state: "working", priority: 190, region: "bottom_non_empty_lines(2)", visible_working: true, contains: ["esc:cancel", "ctrl+.:shortcuts"] },
    { id: "waiting_tool_working", state: "working", priority: 120, visible_working: true, any: [
      { all: [{ contains: ["ctrl+c:cancel", "ctrl+enter:interject"] }, { contains: ["waiting"] }] },
      { line_regex: [String.raw`^\s*[\x{2801}-\x{28FF}]\s+(Run|Read|Search|List)\b`] },
    ] },
    { id: "prompt_hints_idle", state: "idle", priority: 100, region: "bottom_non_empty_lines(2)", visible_idle: true,
      contains: ["ctrl+.:shortcuts"], not: [{ contains: ["esc:cancel"] }, { contains: ["ctrl+c:cancel"] }] },
  ],
};

const pi: AgentManifest = {
  id: "pi", version: "2026.06.10.1", aliases: ["herdr:pi"], rules: [
    { id: "working_literal", state: "working", priority: 100, visible_working: true, contains: ["Working..."] },
  ],
};

// OMP uses its terminal-title run-state separator as the stable fallback:
// `π >` idle, `π <braille>` working, `π !` attention. This behavior is pinned
// by Roost's terminal-title-hub tests and OMP's title-generator contract.
const omp: AgentManifest = {
  id: "omp", version: "roost-2026.08.03.1", rules: [
    { id: "title_attention", state: "blocked", priority: 1200, region: "osc_title", visible_blocker: true, regex: [String.raw`^π\s+!\s`] },
    { id: "title_working", state: "working", priority: 1100, region: "osc_title", visible_working: true, regex: [String.raw`^π\s+[\u2800-\u28ff]\s`] },
    { id: "screen_working", state: "working", priority: 200, region: "bottom_non_empty_lines(4)", visible_working: true,
      any: [{ contains: ["Working..."] }, { contains: ["esc to interrupt"] }] },
    { id: "title_idle", state: "idle", priority: 100, region: "osc_title", visible_idle: true, regex: [String.raw`^π\s+>\s`] },
  ],
};

export const AGENT_MANIFESTS: Readonly<Record<BuiltinAgentId, AgentManifest>> = {
  codex, gemini, opencode, cursor, amp, copilot, droid, grok, pi, omp,
};
