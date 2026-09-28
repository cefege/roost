//! The manifest evaluator: picks a screen or OSC region per rule, runs the
//! rule's contains/regex/all/any/not gates over it, and returns the winning
//! rule's state plus its visible evidence. Ports
//! `apps/worker/src/agents/manifest-engine.ts` (adapted from Herdr
//! `src/detect/manifest.rs` at c7b79294, Apache-2.0). The detector evaluates
//! one compiled manifest per scan; the pinned rules are `agents::manifests`.

use regex::Regex;
use roost_protocol::wire::agent_status::AgentRuntimeState;

use crate::agents::BuiltinAgentId;
use crate::agents::manifest_regex::compile_herdr_regex;

/// A conjunction of checks over one region's text. Every list must hold;
/// `any` holds when empty or when one of its gates does; `not` refuses when
/// any of its gates holds.
#[derive(Debug, Clone, Copy)]
pub struct ManifestGate {
    /// Case-insensitive substrings.
    pub contains: &'static [&'static str],
    /// Patterns over the whole region.
    pub regex: &'static [&'static str],
    /// Patterns each satisfied by at least one line of the region.
    pub line_regex: &'static [&'static str],
    pub all: &'static [ManifestGate],
    pub any: &'static [ManifestGate],
    pub not: &'static [ManifestGate],
}

impl ManifestGate {
    pub const NONE: Self = Self {
        contains: &[],
        regex: &[],
        line_regex: &[],
        all: &[],
        any: &[],
        not: &[],
    };
}

/// Which part of the pane a rule reads. The codex prompt marker is `›` at a
/// line start; a later `•`/`■`/`✗`/`✓` block line means the prompt is history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestRegion {
    WholeRecent,
    OscTitle,
    OscProgress,
    AfterLastPromptMarker,
    WholeRecentWithoutCurrentPromptMarker,
    BottomNonEmptyLines(usize),
    TopNonEmptyLines(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct ManifestRule {
    pub id: &'static str,
    /// `None` is the rule that recognises a screen without judging it.
    pub state: Option<AgentRuntimeState>,
    pub priority: u32,
    pub region: ManifestRegion,
    /// Whether a match is evidence a viewer can see, for its own state.
    pub visible: bool,
    pub skip_state_update: bool,
    pub gate: ManifestGate,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentManifest {
    pub id: BuiltinAgentId,
    pub rules: &'static [ManifestRule],
}

/// What one scan reads. An absent OSC title or progress is empty.
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestDetection {
    /// `None` is "unknown": the screen was recognised but not judged.
    pub state: Option<AgentRuntimeState>,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    pub matched_rule_id: Option<&'static str>,
}

#[derive(Debug)]
struct CompiledGate {
    /// Lowercased once, as the haystack is per rule.
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not: Vec<CompiledGate>,
}

/// One manifest with every pattern compiled, built once by its owner.
#[derive(Debug)]
pub struct CompiledManifest {
    manifest: &'static AgentManifest,
    gates: Vec<CompiledGate>,
}

impl CompiledManifest {
    pub fn compile(manifest: &'static AgentManifest) -> Result<Self, regex::Error> {
        let gates = manifest
            .rules
            .iter()
            .map(|rule| compile_gate(&rule.gate))
            .collect::<Result<_, _>>()?;
        Ok(Self { manifest, gates })
    }

    pub fn id(&self) -> BuiltinAgentId {
        self.manifest.id
    }
}

fn compile_gate(gate: &ManifestGate) -> Result<CompiledGate, regex::Error> {
    let nested = |gates: &[ManifestGate]| {
        gates
            .iter()
            .map(compile_gate)
            .collect::<Result<Vec<_>, _>>()
    };
    Ok(CompiledGate {
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| compile_herdr_regex(pattern))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| compile_herdr_regex(pattern))
            .collect::<Result<_, _>>()?,
        all: nested(gate.all)?,
        any: nested(gate.any)?,
        not: nested(gate.not)?,
    })
}

fn gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    gate.contains
        .iter()
        .all(|needle| lower_text.contains(needle.as_str()))
        && gate.regex.iter().all(|regex| regex.is_match(text))
        && gate
            .line_regex
            .iter()
            .all(|regex| text.split('\n').any(|line| regex.is_match(line)))
        && gate
            .all
            .iter()
            .all(|nested| gate_matches(nested, text, lower_text))
        && (gate.any.is_empty()
            || gate
                .any
                .iter()
                .any(|nested| gate_matches(nested, text, lower_text)))
        && !gate
            .not
            .iter()
            .any(|nested| gate_matches(nested, text, lower_text))
}

/// The highest-priority matching rule's verdict; the earlier rule wins a tie.
/// No match is plain idle with no visible evidence.
pub fn evaluate_manifest(
    manifest: &CompiledManifest,
    input: &DetectionInput<'_>,
) -> ManifestDetection {
    let mut matched: Option<&ManifestRule> = None;
    for (rule, gate) in manifest.manifest.rules.iter().zip(&manifest.gates) {
        let text = select_region(input, rule.region);
        if !gate_matches(gate, text, &text.to_lowercase()) {
            continue;
        }
        if matched.is_none_or(|incumbent| incumbent.priority < rule.priority) {
            matched = Some(rule);
        }
    }
    let Some(rule) = matched else {
        return ManifestDetection {
            state: Some(AgentRuntimeState::Idle),
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_state_update: false,
            matched_rule_id: None,
        };
    };
    ManifestDetection {
        state: rule.state,
        visible_idle: rule.visible && rule.state == Some(AgentRuntimeState::Idle),
        visible_blocker: rule.visible && rule.state == Some(AgentRuntimeState::Blocked),
        visible_working: rule.visible && rule.state == Some(AgentRuntimeState::Working),
        skip_state_update: rule.skip_state_update,
        matched_rule_id: Some(rule.id),
    }
}

fn select_region<'a>(input: &DetectionInput<'a>, region: ManifestRegion) -> &'a str {
    let content = input.screen;
    match region {
        ManifestRegion::WholeRecent => content,
        ManifestRegion::OscTitle => input.osc_title,
        ManifestRegion::OscProgress => input.osc_progress,
        ManifestRegion::AfterLastPromptMarker => {
            let lines: Vec<&str> = content.split('\n').collect();
            match lines.iter().rposition(|line| is_codex_prompt_line(line)) {
                None => content,
                Some(prompt) => &content[line_offset(&lines, prompt + 1).min(content.len())..],
            }
        }
        ManifestRegion::WholeRecentWithoutCurrentPromptMarker => {
            let lines: Vec<&str> = content.split('\n').collect();
            if current_codex_prompt_index(&lines).is_none() {
                content
            } else {
                content
            }
        }
        ManifestRegion::BottomNonEmptyLines(count) => bottom_non_empty_lines(content, count),
        ManifestRegion::TopNonEmptyLines(count) => top_non_empty_lines(content, count),
    }
}

fn is_codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn is_codex_block_line(line: &str) -> bool {
    ['•', '■', '✗', '✓']
        .iter()
        .any(|marker| line.starts_with(*marker))
}

/// The last prompt line, unless a block line below it shows it is history.
fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt = lines.iter().rposition(|line| is_codex_prompt_line(line))?;
    (!lines[prompt + 1..]
        .iter()
        .any(|line| is_codex_block_line(line)))
    .then_some(prompt)
}

/// The byte offset where line `index` starts.
fn line_offset(lines: &[&str], index: usize) -> usize {
    lines.iter().take(index).map(|line| line.len() + 1).sum()
}

/// JavaScript's `trim()`, so a blank row is blank in both runtimes.
fn is_blank(line: &str) -> bool {
    line.chars()
        .all(|character| character.is_whitespace() || character == '\u{feff}')
}

/// From the `count`-th non-blank line counted from the bottom to the end.
fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let mut remaining = count;
    let mut start = None;
    let mut offset = content.len();
    for line in content.rsplit('\n') {
        offset -= line.len();
        if !is_blank(line) {
            start = Some(offset);
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
        offset = offset.saturating_sub(1);
    }
    start.map_or("", |start| &content[start..])
}

/// From the top through the `count`-th non-blank line.
fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let mut remaining = count;
    let mut end = None;
    let mut offset = 0;
    for line in content.split('\n') {
        offset += line.len();
        if !is_blank(line) {
            end = Some(offset);
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
        offset += 1;
    }
    end.map_or("", |end| &content[..end])
}
