//! Ported from oh-my-pi packages/coding-agent/src/lsp/workspace-diagnostics.ts (MIT).
//! This file owns the explicit project checker commands for `diagnostics *`.
//! Each checker runs only when its project marker exists in the cwd.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::outcome::ToolOutcome;
use crate::search_path::resolve_binary;

pub(super) async fn run(
    data_dir: &Path,
    cwd: &Path,
    output: &mpsc::Sender<String>,
    cancel: CancellationToken,
) -> ToolOutcome {
    let checks = [
        (
            "Cargo.toml",
            "cargo",
            vec!["check", "--message-format=short"],
        ),
        ("package.json", "npx", vec!["tsc", "--noEmit"]),
        ("pyproject.toml", "ruff", vec!["check"]),
    ];
    let mut reports = Vec::new();
    for (marker, command, args) in checks {
        if !cwd.join(marker).exists() {
            continue;
        }
        let Some(binary) = resolve_binary(command, cwd, data_dir) else {
            continue;
        };
        let check = Command::new(binary).args(args).current_dir(cwd).output();
        let result = tokio::select! {
            _ = cancel.cancelled() => return ToolOutcome::failure("LSP workspace check cancelled"),
            result = tokio::time::timeout(Duration::from_secs(180), check) => result,
        };
        match result {
            Ok(Ok(result)) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                let _ = output.send(text.clone()).await;
                reports.push(format!(
                    "{command}: exit {}\n{text}",
                    result.status.code().unwrap_or(-1)
                ));
            }
            Ok(Err(error)) => reports.push(format!("{command}: {error}")),
            Err(_) => reports.push(format!("{command}: check timed out")),
        }
    }
    ToolOutcome::success(if reports.is_empty() {
        "No workspace checker matched this project".into()
    } else {
        reports.join("\n")
    })
}
