//! The typed catalog of Roost-owned OMP/Pi integration assets, and the
//! composition that turns each embedded source into the standalone module the
//! agent's loader reads. Ports v2 `apps/worker/src/agents/integration-assets.ts`,
//! `integration-assets.generated.ts` and `standalone-integration.ts`; the
//! installer in [`super::install_integrations`] is the only caller.

use std::io;

use regex::{NoExpand, Regex};

use super::install_proof::{has_integration_ownership, refusal};

/// The shared report transport every integration splices in. It is the ONE
/// transport definition: the embedded sources import it in-repo, and nothing
/// under `~/.omp` or `~/.pi` could resolve that import.
const REPORT_TRANSPORT_SOURCE: &str = include_str!("../../assets/report-transport.ts");

/// v2's `TRANSPORT_MARKER`, including its multi-line import form: `[^}]` and
/// `\s` both cross line ends, and `(?m)` anchors the match to whole lines.
const TRANSPORT_IMPORT_MARKER: &str =
    r#"(?m)^import\s*\{[^}]+\}\s*from\s*"[^"\r\n]*report-transport\.ts";$"#;

/// The agent runtime whose loader directory an asset installs into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentIntegrationRuntime {
    Omp,
    Pi,
}

impl AgentIntegrationRuntime {
    /// Both runtimes, in the order v2 prepares their directories.
    pub const ALL: [AgentIntegrationRuntime; 2] = [Self::Omp, Self::Pi];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omp => "omp",
            Self::Pi => "pi",
        }
    }
}

/// One value per runtime: v2's `Record<AgentIntegrationRuntime, T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByRuntime<T> {
    pub omp: T,
    pub pi: T,
}

impl<T> ByRuntime<T> {
    pub fn get(&self, runtime: AgentIntegrationRuntime) -> &T {
        match runtime {
            AgentIntegrationRuntime::Omp => &self.omp,
            AgentIntegrationRuntime::Pi => &self.pi,
        }
    }
}

/// Which installed asset a report row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentIntegrationAssetId {
    OmpStatus,
    PiStatus,
}

impl AgentIntegrationAssetId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OmpStatus => "omp-status",
            Self::PiStatus => "pi-status",
        }
    }
}

/// One asset Roost installs: where it goes and the token that proves Roost
/// wrote the file already sitting there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIntegrationAssetSpec {
    pub id: AgentIntegrationAssetId,
    pub runtime: AgentIntegrationRuntime,
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
    source: &'static str,
}

/// A filename Roost once installed and now removes, but only when the file
/// there still carries Roost's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredAgentIntegrationSpec {
    pub runtime: AgentIntegrationRuntime,
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
}

/// The complete asset set, in v2's catalog order.
pub const AGENT_INTEGRATION_ASSET_SPECS: [AgentIntegrationAssetSpec; 2] = [
    AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::OmpStatus,
        runtime: AgentIntegrationRuntime::Omp,
        install_filename: "roost-omp-agent-state.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
        source: include_str!("../../assets/integrations/omp/roost-agent-state.ts"),
    },
    AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::PiStatus,
        runtime: AgentIntegrationRuntime::Pi,
        install_filename: "roost-pi-agent-state.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=pi",
        source: include_str!("../../assets/integrations/pi/roost-agent-state.ts"),
    },
];

pub const RETIRED_AGENT_INTEGRATION_SPECS: [RetiredAgentIntegrationSpec; 2] = [
    RetiredAgentIntegrationSpec {
        runtime: AgentIntegrationRuntime::Omp,
        install_filename: "roost-omp-session-api.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
    },
    RetiredAgentIntegrationSpec {
        runtime: AgentIntegrationRuntime::Omp,
        install_filename: "roost-omp-agent-reference.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp-reference",
    },
];

/// An asset with its deployable, self-contained content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedAgentIntegrationAsset {
    pub spec: AgentIntegrationAssetSpec,
    pub content: String,
}

/// Every asset composed with the report transport and proven to carry its own
/// ownership marker, so an install never writes a file it could not later
/// recognise as its own.
pub fn load_agent_integration_assets() -> io::Result<Vec<MaterializedAgentIntegrationAsset>> {
    AGENT_INTEGRATION_ASSET_SPECS
        .iter()
        .map(|spec| {
            let content = compose_standalone_integration(spec.source, REPORT_TRANSPORT_SOURCE)?;
            if !has_integration_ownership(&content, spec.ownership_marker) {
                return Err(refusal(format!(
                    "agent integration asset {} lost its ownership marker",
                    spec.id.as_str()
                )));
            }
            Ok(MaterializedAgentIntegrationAsset {
                spec: *spec,
                content,
            })
        })
        .collect()
}

/// Replace an integration source's report-transport import with the transport
/// module body. A source whose import is gone is refused: the composed asset
/// would otherwise ship without its reporter and fail only on the user's machine.
pub fn compose_standalone_integration(
    integration_source: &str,
    transport_source: &str,
) -> io::Result<String> {
    let marker = Regex::new(TRANSPORT_IMPORT_MARKER).map_err(io::Error::other)?;
    if !marker.is_match(integration_source) {
        return Err(refusal(
            "integration source lost its report-transport import marker".to_owned(),
        ));
    }
    Ok(marker
        .replace(integration_source, NoExpand(transport_source))
        .into_owned())
}
