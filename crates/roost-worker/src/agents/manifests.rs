//! The pinned screen rules for the ten built-in agents, and the compiled set a
//! detector owns. Ports `apps/worker/src/agents/manifests.ts` (pinned/adapted
//! from Herdr `src/detect/manifests/*.toml` at c7b79294, Apache-2.0; `\A`/`\z`
//! are written `^`/`$`, which bind to the whole region here too). Read by
//! `agents::detector` through [`AgentManifests`], built once at boot. Tables
//! skip rustfmt: one rule per line is what makes them comparable to Herdr's.

use crate::agents::BuiltinAgentId;
use crate::agents::manifest_engine::ManifestRegion::{
    AfterLastPromptMarker, BottomNonEmptyLines, OscProgress, OscTitle, TopNonEmptyLines,
    WholeRecentWithoutCurrentPromptMarker,
};
use crate::agents::manifest_engine::{AgentManifest, CompiledManifest, ManifestGate};
use crate::agents::manifest_syntax::{
    all, any, blocked, contains, idle, line_regex, manifest, regex, unknown, working,
};
use crate::agents::manifests_claude::CLAUDE;

const BRAILLE_SPINNER: &str = r"(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)";

/// codex 2026.09.05.1
#[rustfmt::skip]
const CODEX: AgentManifest = manifest(BuiltinAgentId::Codex, &[
    blocked("osc_title_blocked", 1100, contains(&["Action Required"])).at(OscTitle).visible(),
    working("osc_title_working", 1050, regex(&[BRAILLE_SPINNER])).at(OscTitle).visible(),
    unknown("transcript_viewer", 1000, ManifestGate {
        contains: &["↑/↓ to scroll", "pgup/pgdn to", "home/end to jump", "q to quit"],
        any: &[contains(&["esc to edit prev"]), contains(&["esc/← to edit prev"])],
        ..ManifestGate::NONE
    }).at(AfterLastPromptMarker).skipping_state_update(),
    blocked("trust_directory", 950, all(&[
        regex(&[r"^> You are in [^\r\n]+(?:\r?\n|$)"]),
        regex(&[r"Do\s+you\s+trust\s+the\s+contents\s+of\s+this\s+directory\?"]),
    ])).at(TopNonEmptyLines(20)).visible(),
    blocked("startup_update", 950, ManifestGate {
        contains: &["Update available!", "Update now"],
        regex: &[r"Skip\s+until\s+next\s+version", r"Press enter to continue\s*$"],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(20)).visible(),
    blocked("live_strong_blocker", 900, any(&[
        contains(&["press enter to confirm or esc to cancel"]),
        contains(&["enter to submit answer"]),
        contains(&["enter to submit all"]),
        contains(&["allow command?"]),
    ])).at(AfterLastPromptMarker).visible(),
    blocked("weak_blocker", 600, any(&[
        contains(&["[y/n]"]),
        contains(&["yes (y)"]),
        ManifestGate { contains: &["do you want to"], any: &[contains(&["yes"]), contains(&["❯"])], ..ManifestGate::NONE },
        ManifestGate { contains: &["would you like to"], any: &[contains(&["yes"]), contains(&["❯"])], ..ManifestGate::NONE },
    ])).at(WholeRecentWithoutCurrentPromptMarker),
    working("screen_working_fallback", 500, ManifestGate {
        line_regex: &[r"^[•◦]\s+Working \([^)]*esc to interrupt\)(?: · .*)?$"],
        not: &[contains(&["■ Conversation interrupted"])],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(3)).visible(),
    idle("osc_title_idle", 100, ManifestGate {
        regex: &[r"\S"],
        not: &[regex(&[BRAILLE_SPINNER]), contains(&["Action Required"])],
        ..ManifestGate::NONE
    }).at(OscTitle).visible(),
]);

/// gemini 2026.06.10.1
#[rustfmt::skip]
const GEMINI: AgentManifest = manifest(BuiltinAgentId::Gemini, &[
    blocked("apply_or_allow_change", 300, any(&[
        contains(&["│ Apply this change"]),
        contains(&["│ Allow execution"]),
        all(&[contains(&["yes"]), any(&[
            contains(&["waiting for user confirmation"]),
            contains(&["│ Do you want to proceed"]),
            contains(&["do you want to proceed?"]),
        ])]),
        line_regex(&[r"(?i)^\s*❯.*(yes|allow)"]),
    ])).visible(),
    working("esc_cancel_working", 100, contains(&["esc to cancel"])).visible(),
]);

/// opencode 2026.06.10.1
#[rustfmt::skip]
const OPENCODE: AgentManifest = manifest(BuiltinAgentId::OpenCode, &[
    blocked("permission_required", 300, any(&[
        contains(&["△ Permission required"]),
        ManifestGate {
            contains: &["esc dismiss"],
            any: &[contains(&["enter confirm"]), contains(&["enter submit"]), contains(&["enter toggle"])],
            all: &[any(&[contains(&["↑↓ select"]), contains(&["⇆ tab"])])],
            ..ManifestGate::NONE
        },
    ])).visible(),
    working("interrupt_hint_working", 110, any(&[
        contains(&["esc to interrupt"]),
        contains(&["ctrl+c to interrupt"]),
        contains(&["press esc to interrupt"]),
        line_regex(&[r"(?i).*opencode.*esc (again to )?interrupt"]),
    ])).visible(),
    working("progress_bar_working", 100, regex(&["(■|⬝){4,}"])).visible(),
]);

/// cursor 2026.08.03.1
#[rustfmt::skip]
const CURSOR: AgentManifest = manifest(BuiltinAgentId::Cursor, &[
    blocked("write_file_approval", 320, ManifestGate {
        contains: &["write to this file?", "proceed (y)"],
        any: &[contains(&["reject & propose changes"]), contains(&["esc or n or p"]), contains(&["add write("])],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(8)).visible(),
    blocked("approval_prompt", 300, any(&[
        ManifestGate {
            contains: &["waiting for approval", "run this command?"],
            any: &[contains(&["run (once) (y)"]), contains(&["skip (esc or n)"])],
            ..ManifestGate::NONE
        },
        contains(&["(y) (enter)"]),
        line_regex(&[r"(?i)^\s*allow .*\(y\)"]),
        contains(&["keep (n)"]),
        contains(&["skip (esc or n)"]),
        line_regex(&[r"(?i)^\s*(?:→\s*)?run .*\(y\)"]),
    ])).visible(),
    working("stop_hint_working", 100, contains(&["ctrl+c to stop"])).at(BottomNonEmptyLines(6)).visible(),
    working("background_task_status_working", 95, line_regex(&[r"(?i)\b[1-9][0-9]*\s+background\s+tasks?\b"]))
        .at(BottomNonEmptyLines(5)).visible(),
    working("spinner_working", 90, line_regex(&[r"^\s*(⬡|⬢|[\u2800-\u28FF]+)\s+\p{Alphabetic}+\w*ing\b"]))
        .at(BottomNonEmptyLines(8)).visible(),
]);

const AMP_SPINNER_TITLE: &str = r"^[\x{2800}-\x{28FF}] ";

/// amp 2026.07.09.1
#[rustfmt::skip]
const AMP: AgentManifest = manifest(BuiltinAgentId::Amp, &[
    blocked("osc_title_plugin_confirmation_blocked", 1100, contains(&["Plugin confirmation needed"])).at(OscTitle).visible(),
    working("osc_title_working", 1050, regex(&[AMP_SPINNER_TITLE])).at(OscTitle).visible(),
    blocked("approval_footer", 300, any(&[
        contains(&["waiting for approval"]),
        contains(&["invoke tool"]),
        contains(&["run this command?"]),
        contains(&["allow editing file:"]),
        contains(&["allow creating file:"]),
        contains(&["confirm tool call"]),
        ManifestGate {
            contains: &["approve"],
            any: &[
                contains(&["allow all for this session"]),
                contains(&["allow all for every session"]),
                contains(&["allow file for every session"]),
                contains(&["deny with feedback"]),
            ],
            ..ManifestGate::NONE
        },
    ])).visible(),
    working("status_footer_working", 200, line_regex(&[r"(?i)^\s*╰\s+\S+\s+(thinking|streaming|running tools|waiting)\s+─"]))
        .at(BottomNonEmptyLines(5)).visible(),
    working("esc_cancel_working", 100, contains(&["esc to cancel"])).visible(),
    idle("osc_title_idle", 50, ManifestGate {
        contains: &[" - amp - "],
        not: &[regex(&[AMP_SPINNER_TITLE]), contains(&["Plugin confirmation needed"])],
        ..ManifestGate::NONE
    }).at(OscTitle).visible(),
]);

/// copilot 2026.08.29.1
#[rustfmt::skip]
const COPILOT: AgentManifest = manifest(BuiltinAgentId::Copilot, &[
    blocked("selection_blocker", 300, all(&[
        any(&[contains(&["esc to cancel"]), contains(&["esc cancel"])]),
        any(&[
            contains(&["enter to select"]),
            contains(&["enter to confirm"]),
            contains(&["enter to submit"]),
            contains(&["enter accept"]),
        ]),
    ])).visible(),
    working("background_agents_working", 110, line_regex(&[r"^\s*◎\s+Waiting for background agents(?:\s|·|$)"]))
        .at(BottomNonEmptyLines(6)).visible(),
    working("working_cancel_hint", 100, any(&[
        contains(&["esc to cancel"]),
        contains(&["esc cancel"]),
        contains(&["esc again to cancel"]),
        contains(&["esc interrupt"]),
    ])).visible(),
]);

/// droid 2026.06.10.1
#[rustfmt::skip]
const DROID: AgentManifest = manifest(BuiltinAgentId::Droid, &[
    blocked("execute_selection_blocker", 300, ManifestGate {
        contains: &["enter to select", "esc to cancel"],
        any: &[contains(&["↑↓ to navigate"]), contains(&["use ↑↓ to navigate"])],
        all: &[any(&[contains(&["> yes, allow"]), contains(&["> no, cancel"])])],
        ..ManifestGate::NONE
    }).visible(),
    blocked("selection_menu_blocker", 290, ManifestGate {
        contains: &["enter select", "esc cancel"],
        any: &[contains(&["↑/↓ navigate"]), contains(&["↑↓ navigate"])],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(8)).visible(),
    working("spinner_stop_working", 110, ManifestGate {
        contains: &["esc to stop"],
        line_regex: &[r"^\s*[\u2800-\u28FF]"],
        ..ManifestGate::NONE
    }).visible(),
    working("stop_hint_working", 100, contains(&["esc to stop"])).visible(),
]);

/// grok 2026.07.16.2
#[rustfmt::skip]
const GROK: AgentManifest = manifest(BuiltinAgentId::Grok, &[
    blocked("osc_title_blocked", 1300, contains(&["Action Required"])).at(OscTitle).visible(),
    blocked("option_dialog_blocked", 1200, line_regex(&[r"^\s*┃\s+[0-9a-z]+\s+\([●○]\)\s"])).visible(),
    blocked("permission_hints_blocked", 1190, contains(&[":select", "ctrl+o:yolo", "ctrl+c:cancel"]))
        .at(BottomNonEmptyLines(2)).visible(),
    blocked("question_dialog_hints_blocked", 1185, contains(&["tab:scrollback", "shift+x:dismiss"]))
        .at(BottomNonEmptyLines(2)).visible(),
    blocked("permission_scope_selector", 1180, ManifestGate {
        contains: &["yes, proceed", "no, reject"],
        any: &[contains(&["use ← → to choose permission whitelist scope"]), contains(&["←/→:scope"])],
        ..ManifestGate::NONE
    }).visible(),
    working("background_work_chip_working", 1170, line_regex(&[r"[⋅:⸬⁙.·]\s+[1-9][0-9]*\s+│"]))
        .at(TopNonEmptyLines(1)).visible(),
    working("osc_progress_working", 1150, regex(&["^4;1;-1$"])).at(OscProgress).visible(),
    idle("osc_title_idle", 1100, ManifestGate {
        regex: &["(?:^| - )grok$"],
        not: &[regex(&[r"[\x{2800}-\x{28FF}]"])],
        ..ManifestGate::NONE
    }).at(OscTitle).visible(),
    working("osc_title_working", 1000, regex(&[r"\S"])).at(OscTitle).visible(),
    idle("osc_progress_idle", 950, regex(&["^4;0;0$"])).at(OscProgress).visible(),
    working("spinner_status_working", 200, line_regex(&[r"^\s*[\x{2801}-\x{28FF}]\s.*\[stop\]\s*$"])).visible(),
    working("esc_cancel_hints_working", 190, contains(&["esc:cancel", "ctrl+.:shortcuts"]))
        .at(BottomNonEmptyLines(2)).visible(),
    working("waiting_tool_working", 120, any(&[
        all(&[contains(&["ctrl+c:cancel", "ctrl+enter:interject"]), contains(&["waiting"])]),
        line_regex(&[r"^\s*[\x{2801}-\x{28FF}]\s+(Run|Read|Search|List)\b"]),
    ])).visible(),
    idle("prompt_hints_idle", 100, ManifestGate {
        contains: &["ctrl+.:shortcuts"],
        not: &[contains(&["esc:cancel"]), contains(&["ctrl+c:cancel"])],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(2)).visible(),
]);

/// pi 2026.06.10.1
#[rustfmt::skip]
const PI: AgentManifest = manifest(BuiltinAgentId::Pi, &[
    working("working_literal", 100, contains(&["Working..."])).visible(),
]);

/// roost-2026.08.03.1. OMP's terminal-title run-state separator is the stable
/// signal: `π >` idle, `π <braille>` working, `π !` attention.
#[rustfmt::skip]
const OMP: AgentManifest = manifest(BuiltinAgentId::Omp, &[
    blocked("title_attention", 1200, regex(&[r"^π\s+!\s"])).at(OscTitle).visible(),
    working("title_working", 1100, regex(&[r"^π\s+[\u2800-\u28ff]\s"])).at(OscTitle).visible(),
    working("screen_working", 200, any(&[contains(&["Working..."]), contains(&["esc to interrupt"])]))
        .at(BottomNonEmptyLines(4)).visible(),
    idle("title_idle", 100, regex(&[r"^π\s+>\s"])).at(OscTitle).visible(),
]);

/// The pinned manifest for one agent.
pub fn agent_manifest(agent: BuiltinAgentId) -> &'static AgentManifest {
    match agent {
        BuiltinAgentId::Codex => &CODEX,
        BuiltinAgentId::Claude => &CLAUDE,
        BuiltinAgentId::Gemini => &GEMINI,
        BuiltinAgentId::OpenCode => &OPENCODE,
        BuiltinAgentId::Cursor => &CURSOR,
        BuiltinAgentId::Amp => &AMP,
        BuiltinAgentId::Copilot => &COPILOT,
        BuiltinAgentId::Droid => &DROID,
        BuiltinAgentId::Grok => &GROK,
        BuiltinAgentId::Pi => &PI,
        BuiltinAgentId::Omp => &OMP,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the pinned {agent} manifest does not compile: {source}")]
pub struct ManifestCompileError {
    pub agent: &'static str,
    pub source: regex::Error,
}

/// Every pinned manifest compiled once, indexed by agent.
#[derive(Debug)]
pub struct AgentManifests {
    compiled: [CompiledManifest; BuiltinAgentId::ALL.len()],
}

impl AgentManifests {
    pub fn pinned() -> Result<Self, ManifestCompileError> {
        let compile = |agent: BuiltinAgentId| {
            CompiledManifest::compile(agent_manifest(agent)).map_err(|source| {
                ManifestCompileError {
                    agent: agent.as_str(),
                    source,
                }
            })
        };
        // Declaration order, which is the discriminant order `get` indexes by.
        Ok(Self {
            compiled: [
                compile(BuiltinAgentId::Codex)?,
                compile(BuiltinAgentId::Claude)?,
                compile(BuiltinAgentId::Gemini)?,
                compile(BuiltinAgentId::OpenCode)?,
                compile(BuiltinAgentId::Cursor)?,
                compile(BuiltinAgentId::Amp)?,
                compile(BuiltinAgentId::Copilot)?,
                compile(BuiltinAgentId::Droid)?,
                compile(BuiltinAgentId::Grok)?,
                compile(BuiltinAgentId::Pi)?,
                compile(BuiltinAgentId::Omp)?,
            ],
        })
    }

    pub fn get(&self, agent: BuiltinAgentId) -> &CompiledManifest {
        &self.compiled[agent as usize]
    }
}
