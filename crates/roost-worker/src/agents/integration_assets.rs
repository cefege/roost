//! Roost-owned OMP integration assets and their standalone composition.
//! The installer loads embedded sources with the report transport spliced in,
//! then verifies the same ownership marker used by install-time guards.

use std::io;

use regex::{NoExpand, Regex};

use super::install_proof::{has_integration_ownership, refusal};

/// The report transport spliced into the installed OMP extension, whose loader
/// cannot resolve the in-repository import.
const REPORT_TRANSPORT_SOURCE: &str = include_str!("../../assets/report-transport.ts");

/// v2's `TRANSPORT_MARKER`, including its multi-line import form: `[^}]` and
/// `\s` both cross line ends, and `(?m)` anchors the match to whole lines.
const TRANSPORT_IMPORT_MARKER: &str =
    r#"(?m)^import\s*\{[^}]+\}\s*from\s*"[^"\r\n]*report-transport\.ts";$"#;

/// Which installed asset a report row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentIntegrationAssetId {
    OmpStatus,
}

impl AgentIntegrationAssetId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OmpStatus => "omp-status",
        }
    }
}

/// One asset Roost installs: where it goes and the token that proves Roost
/// wrote the file already sitting there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIntegrationAssetSpec {
    pub id: AgentIntegrationAssetId,
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
    source: &'static str,
}

/// A filename Roost once installed and now removes, but only when the file
/// there still carries Roost's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredAgentIntegrationSpec {
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
}

/// The complete supported OMP integration asset set.
pub const AGENT_INTEGRATION_ASSET_SPECS: [AgentIntegrationAssetSpec; 1] =
    [AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::OmpStatus,
        install_filename: "roost-omp-agent-state.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
        source: include_str!("../../assets/integrations/omp/roost-agent-state.ts"),
    }];

pub const RETIRED_AGENT_INTEGRATION_SPECS: [RetiredAgentIntegrationSpec; 2] = [
    RetiredAgentIntegrationSpec {
        install_filename: "roost-omp-session-api.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
    },
    RetiredAgentIntegrationSpec {
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
