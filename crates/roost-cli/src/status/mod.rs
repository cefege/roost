//! `roost status` — the current-state gate: local services, the coordinator,
//! the declared front door, and the fleet roster, in one screen. Owns the
//! command's arguments and its exit code; status/collect.rs gathers and
//! status/render.rs prints. Called by the crate's dispatcher and by the
//! output-shape tests, which run it against fixtures with no services running.
//!
//! Exit 0 when both services are loaded, the coordinator answers, and a
//! DECLARED front door answers; exit 1 otherwise. An undeclared front door is a
//! valid same-origin install and never fails the gate. The fleet rows are not
//! part of the exit code — see status/render.rs.

pub mod collect;
pub mod http_probe;
pub mod inventory;
pub mod render;
pub mod report;
pub mod service_definition;
pub mod service_probe;
pub mod update_state;

use std::process::ExitCode;

use clap::Args;
use roost_host::ProcessEnv;

use crate::command_error::CommandFailure;
use crate::wall_clock;

#[derive(Debug, Args)]
#[command(about = "Health readout: local services, coordinator, front door, workers")]
pub struct StatusArgs {
    /// Speak about this front door instead of the one the installed service
    /// definition declares. For a machine whose unit names a URL that has
    /// since moved, without editing the install to find out.
    #[arg(long, value_name = "ORIGIN")]
    pub endpoint: Option<String>,
}

pub async fn run(args: &StatusArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let now_ms = wall_clock::now_ms();
    let collected = collect::collect(&collect::StatusContext {
        env: &env,
        platform,
        now_ms,
        endpoint_override: args.endpoint.clone(),
    })
    .await?;
    let labels = render::ServiceLabels {
        coord: &collected.coord_label,
        worker: &collected.worker_label,
    };
    println!(
        "{}",
        render::render_status_report(&collected.report, labels, now_ms)
    );
    Ok(if render::status_report_is_healthy(&collected.report) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
