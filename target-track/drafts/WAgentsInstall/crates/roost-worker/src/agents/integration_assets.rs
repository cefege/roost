//! The Roost-owned agent integration assets: the typed catalog, the embedded
//! TypeScript plugin sources, and their composition into the standalone files
//! the installer writes. Ports v2 `apps/worker/src/agents/integration-assets.ts`,
//! `integration-assets.generated.ts` and `standalone-integration.ts`, and embeds
//! `integrations/{omp,pi}/*.ts` plus `report-transport.ts` verbatim from
//! `crates/roost-worker/assets/`. Read by `agents::install_integrations`.

use regex::{NoExpand, Regex};

use crate::agents::install_proof::{IntegrationInstallError, has_integration_ownership};

/// One asset of the set. The wire spelling is the id a log line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentIntegrationAssetId {
    OmpStatus,
    OmpReference,
    PiStatus,
}

impl AgentIntegrationAssetId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OmpStatus => "omp-status",
            Self::OmpReference => "omp-reference",
            Self::PiStatus => "pi-status",
        }
    }
}

/// The agent whose extension directory an asset installs into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentIntegrationRuntime {
    Omp,
    Pi,
}

impl AgentIntegrationRuntime {
    /// Both runtimes, in the order directories are prepared and cleaned up.
    pub const ALL: [Self; 2] = [Self::Omp, Self::Pi];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omp => "omp",
            Self::Pi => "pi",
        }
    }
}

/// One value per runtime, so a lookup by runtime cannot miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerRuntime<T> {
    pub omp: T,
    pub pi: T,
}

impl<T> PerRuntime<T> {
    pub fn get(&self, runtime: AgentIntegrationRuntime) -> &T {
        match runtime {
            AgentIntegrationRuntime::Omp => &self.omp,
            AgentIntegrationRuntime::Pi => &self.pi,
        }
    }
}

/// A catalog row: which runtime loads the asset, the file name it is installed
/// under, the marker that proves an installed file is Roost's, and the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIntegrationAssetSpec {
    pub id: AgentIntegrationAssetId,
    pub runtime: AgentIntegrationRuntime,
    pub source: &'static str,
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
}

/// A file name an earlier release installed and this one removes when, and
/// only when, the file there still carries Roost's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredAgentIntegrationSpec {
    pub runtime: AgentIntegrationRuntime,
    pub install_filename: &'static str,
    pub ownership_marker: &'static str,
}

/// The complete asset set, in install and report order.
pub const AGENT_INTEGRATION_ASSET_SPECS: [AgentIntegrationAssetSpec; 3] = [
    AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::OmpStatus,
        runtime: AgentIntegrationRuntime::Omp,
        source: include_str!("../../assets/integrations/omp/roost-agent-state.ts"),
        install_filename: "roost-omp-agent-state.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
    },
    AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::OmpReference,
        runtime: AgentIntegrationRuntime::Omp,
        source: include_str!("../../assets/integrations/omp/roost-agent-reference.ts"),
        install_filename: "roost-omp-agent-reference.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp-reference",
    },
    AgentIntegrationAssetSpec {
        id: AgentIntegrationAssetId::PiStatus,
        runtime: AgentIntegrationRuntime::Pi,
        source: include_str!("../../assets/integrations/pi/roost-agent-state.ts"),
        install_filename: "roost-pi-agent-state.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=pi",
    },
];

pub const RETIRED_AGENT_INTEGRATION_SPECS: [RetiredAgentIntegrationSpec; 1] =
    [RetiredAgentIntegrationSpec {
        runtime: AgentIntegrationRuntime::Omp,
        install_filename: "roost-omp-session-api.ts",
        ownership_marker: "ROOST_INTEGRATION_ID=omp",
    }];

/// The shared delivery transport every integration imports in-repo. Nothing in
/// an agent's config directory can resolve that import, so the installed form
/// of each integration carries this module's text in its place.
const REPORT_TRANSPORT_SOURCE: &str = include_str!("../../assets/report-transport.ts");

/// The integration's import of the transport. `[^}]` spans lines on purpose:
/// the reference integration imports three names over five lines.
const TRANSPORT_IMPORT_PATTERN: &str =
    r#"(?m)^import\s*\{[^}]+\}\s*from\s*"[^"\r\n]*report-transport\.ts";$"#;

/// An asset in the exact form the installer writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedAgentIntegrationAsset {
    pub spec: AgentIntegrationAssetSpec,
    pub content: String,
}

/// Compose every asset into its standalone form and prove each still carries
/// its ownership marker. A failure here fails the whole pass: installing a
/// file Roost could never again recognise as its own is worse than none.
pub fn load_agent_integration_assets()
-> Result<Vec<MaterializedAgentIntegrationAsset>, IntegrationInstallError> {
    let transport_import = Regex::new(TRANSPORT_IMPORT_PATTERN).map_err(|error| {
        IntegrationInstallError::refused(format!(
            "the transport import pattern is invalid: {error}"
        ))
    })?;
    AGENT_INTEGRATION_ASSET_SPECS
        .into_iter()
        .map(|spec| {
            let content = compose_standalone_integration(
                &transport_import,
                spec.source,
                REPORT_TRANSPORT_SOURCE,
            )?;
            if !has_integration_ownership(&content, spec.ownership_marker) {
                return Err(IntegrationInstallError::refused(format!(
                    "agent integration asset {} lost its ownership marker",
                    spec.id.as_str()
                )));
            }
            Ok(MaterializedAgentIntegrationAsset {
                spec,
                content,
            })
        })
        .collect()
}

/// Replace the integration's transport import with the transport module itself.
/// A source whose import is gone is refused: the composed file would silently
/// lose its reporter.
fn compose_standalone_integration(
    transport_import: &Regex,
    integration_source: &str,
    transport_source: &str,
) -> Result<String, IntegrationInstallError> {
    if !transport_import.is_match(integration_source) {
        return Err(IntegrationInstallError::refused(
            "integration source lost its report-transport import marker",
        ));
    }
    Ok(transport_import
        .replacen(integration_source, 1, NoExpand(transport_source))
        .into_owned())
}
