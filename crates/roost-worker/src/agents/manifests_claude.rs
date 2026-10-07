//! The Claude Code screen rules, pinned from Herdr
//! `src/detect/manifests/claude.toml` (Apache-2.0) at the same revision the
//! other pinned manifests were adapted at. Kept beside [`super::manifests`]
//! rather than in it so each agent's table stays under the file-size cap.
//! Read by `agents::manifests` through [`CLAUDE`]; tables skip rustfmt.

use crate::agents::BuiltinAgentId;
use crate::agents::manifest_engine::AgentManifest;
use crate::agents::manifest_engine::ManifestGate;
use crate::agents::manifest_engine::ManifestRegion::{
    BottomNonEmptyLines, OscProgress, OscTitle, WholeRecent, WholeRecentWithoutCurrentPromptMarker,
};
use crate::agents::manifest_syntax::{
    all, any, blocked, contains, idle, line_regex, manifest, regex, unknown, working,
};

/// claude 2026.09.11.1, pinned from Herdr `src/detect/manifests/claude.toml`
/// (Apache-2.0). Herdr's `after_last_horizontal_rule` and
/// `last_non_empty_above_prompt_box` regions have no engine counterpart, so
/// those rules read the whole recent region; their `esc to cancel` gates are
/// what keep a transcript echo from impersonating a live prompt.
#[rustfmt::skip]
pub(super) const CLAUDE: AgentManifest = manifest(BuiltinAgentId::Claude, &[
    working("osc_title_working", 1100, regex(&[
        r"^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}] ",
    ])).at(OscTitle).visible(),
    working("btw_overlay_working", 975, line_regex(&[
        r"^\s*/btw(?:\s|$)",
        r"(?i)esc to close\s*$",
    ])).at(BottomNonEmptyLines(5)).visible(),
    working("live_turn_working", 970, any(&[
        ManifestGate {
            line_regex: &[r"^\s*[⏸⏵].*esc to interrupt(?:\s|·|$)"],
            ..ManifestGate::NONE
        },
        ManifestGate {
            line_regex: &[r"^\s*[\x{002A}\x{00B7}\x{2722}\x{2733}\x{2736}\x{273B}\x{273D}]\s+\S.*…(?:\s+\(\d+[smh](?:\s|·)|\s*$)"],
            ..ManifestGate::NONE
        },
    ])).at(BottomNonEmptyLines(12)).visible(),
    blocked("live_blocked_form", 980, all(&[
        contains(&["esc to cancel"]),
        any(&[
            contains(&["enter to confirm"]),
            ManifestGate {
                contains: &["enter to select"],
                any: &[
                    contains(&["tab/arrow keys to navigate"]),
                    contains(&["arrow keys to navigate"]),
                    contains(&["arrows to navigate"]),
                    contains(&["↑/↓ to navigate"]),
                    contains(&["↑↓ to navigate"]),
                ],
                ..ManifestGate::NONE
            },
        ]),
    ])).at(WholeRecent).visible(),
    blocked("dynamic_workflow_prompt", 980, contains(&[
        "run a dynamic workflow?",
        "esc to cancel",
    ])).at(WholeRecent).visible(),
    blocked("mcp_elicitation_prompt", 980, all(&[
        contains(&["esc to cancel"]),
        regex(&[r#"(?i)^\s*MCP server ["\x{201C}].+["\x{201D}] requests your input\s*$"#]),
        any(&[
            ManifestGate { line_regex: &[r"^\s*❯?\s*Accept\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"^\s*❯?\s*Decline\b"], ..ManifestGate::NONE },
        ]),
    ])).at(WholeRecent).visible(),
    unknown("transcript_viewer", 1000, ManifestGate {
        contains: &["showing detailed transcript"],
        any: &[
            contains(&["ctrl+o", "to toggle"]),
            contains(&["ctrl+e", "show all"]),
            contains(&["ctrl+e", "collapse"]),
            contains(&["↑↓ scroll"]),
            contains(&["? for shortcuts"]),
        ],
        ..ManifestGate::NONE
    }).at(BottomNonEmptyLines(3)).skipping_state_update(),
    idle("live_prompt_box", 950, ManifestGate {
        line_regex: &[r"^\s*❯"],
        not: &[
            contains(&["enter to select"]),
            contains(&["esc to cancel"]),
            contains(&["tab/arrow keys"]),
            contains(&["arrow keys to navigate"]),
            contains(&["↑/↓ to navigate"]),
        ],
        ..ManifestGate::NONE
    }).at(WholeRecent).visible(),
    unknown("model_picker_menu", 900, ManifestGate {
        contains: &["select model", "enter to set as default", "esc to cancel"],
        not: &[
            contains(&["do you want to proceed?"]),
            contains(&["enter to select"]),
        ],
        ..ManifestGate::NONE
    }).at(WholeRecent).skipping_state_update(),
    blocked("bash_permission_prompt", 1050, ManifestGate {
        contains: &["do you want to proceed?"],
        any: &[
            contains(&["bash command"]),
            contains(&["bash("]),
            contains(&["contains expansion"]),
            contains(&["tab to amend"]),
            contains(&["ctrl+e to explain"]),
        ],
        all: &[any(&[
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*yes\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*1\.\s*yes\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*2\.\s*yes\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*2\.\s*no\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*3\.\s*no\b"], ..ManifestGate::NONE },
        ])],
        ..ManifestGate::NONE
    }).at(WholeRecent).visible(),
    blocked("generic_permission_prompt", 1040, ManifestGate {
        contains: &["do you want to proceed?", "esc to cancel"],
        all: &[any(&[
            ManifestGate { line_regex: &[r"(?i)^\s*❯?\s*1\.\s*yes\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*2\.\s*yes\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*2\.\s*no\b"], ..ManifestGate::NONE },
            ManifestGate { line_regex: &[r"(?i)^\s*3\.\s*no\b"], ..ManifestGate::NONE },
        ])],
        ..ManifestGate::NONE
    }).at(WholeRecent).visible(),
    // Herdr reads this rule over `whole_recent` while the composer idle rule
    // reads `prompt_box_body`, a region this engine has no counterpart for;
    // here both read the whole recent screen, so the fallback must outrank the
    // composer or a live "do you want to" prompt without Enter hints is scored
    // idle. The bare-`❯` not-gate is what keeps a settled composer idle.
    blocked("legacy_no_prompt_blocker", 960, ManifestGate {
        any: &[
            ManifestGate {
                contains: &["do you want to"],
                any: &[contains(&["yes"]), contains(&["❯"])],
                ..ManifestGate::NONE
            },
            ManifestGate {
                contains: &["would you like to"],
                any: &[contains(&["yes"]), contains(&["❯"])],
                ..ManifestGate::NONE
            },
            contains(&["waiting for permission"]),
            contains(&["do you want to allow this connection?"]),
            contains(&["tab to amend"]),
            contains(&["ctrl+e to explain"]),
            contains(&["do you want to proceed?", "esc to cancel"]),
            contains(&["review your answers"]),
            contains(&["skip interview and plan immediately"]),
        ],
        not: &[regex(&[r"(?m)^\s*❯\s*$"])],
        ..ManifestGate::NONE
    }).at(WholeRecentWithoutCurrentPromptMarker),
    idle("osc_title_idle", 250, regex(&[r"^\x{2733} "])).at(OscTitle).visible(),
    idle("osc_progress_idle", 250, regex(&[r"^4;0"])).at(OscProgress).visible(),
]);
