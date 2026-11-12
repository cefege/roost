//! OMP's screen and title rules, compiled once at worker boot.
//! `agents::detector` evaluates them only after the process scanner proves
//! an OMP identity; integration reports take precedence over screen evidence.

use crate::agents::BuiltinAgentId;
use crate::agents::manifest_engine::ManifestRegion::{BottomNonEmptyLines, OscTitle};
use crate::agents::manifest_engine::{AgentManifest, CompiledManifest};
use crate::agents::manifest_syntax::{any, blocked, contains, idle, manifest, regex, working};

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
            compiled: [compile(BuiltinAgentId::Omp)?],
        })
    }

    pub fn get(&self, agent: BuiltinAgentId) -> &CompiledManifest {
        &self.compiled[agent as usize]
    }
}
