//! Ported from oh-my-pi packages/coding-agent/src/lsp/tool.ts (MIT).
//! This file owns LSP request dispatch and model-facing text results.
//! Lifecycle, document syncing, response rendering and edits use sibling modules.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::documents::{sync_document, wait_for_push_diagnostics};
use super::edits::{apply_workspace_edit, workspace_edit_preview};
use super::manager::LspManager;
use super::render::{diagnostics_text, hover_text, locations_text, symbols_text};
use super::servers::selected_servers;
use super::types::RunningServer;
use super::workspace_checks;
use crate::outcome::ToolOutcome;
use roost_protocol::wire::agent_chat::{LspAction, LspArgs};

impl LspManager {
    pub async fn run_tool(
        &self,
        cwd: &Path,
        args: LspArgs,
        output: &mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        if cancel.is_cancelled() {
            return ToolOutcome::failure("LSP request cancelled");
        }
        if args.action == LspAction::Status {
            return ToolOutcome::success(self.status_text(cwd).await);
        }
        if args.action == LspAction::Diagnostics && args.file.as_deref() == Some("*") {
            return workspace_checks::run(&self.data_dir, cwd, output, cancel).await;
        }
        let Some(file) = args.file.as_deref() else {
            return ToolOutcome::failure("lsp requires file");
        };
        let path = resolve_path(cwd, file);
        let servers = selected_servers(cwd, &path);
        if servers.is_empty() {
            return ToolOutcome::failure(format!(
                "No language server found for {}",
                relative(cwd, &path)
            ));
        }
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return ToolOutcome::failure(format!("Unable to read {}", relative(cwd, &path)));
        };
        if args.action == LspAction::Rename && args.new_name.as_deref().is_none_or(str::is_empty) {
            return ToolOutcome::failure("lsp rename requires new_name");
        }
        let mut results = Vec::new();
        for (name, config, root) in servers {
            if cancel.is_cancelled() {
                return ToolOutcome::failure("LSP request cancelled");
            }
            let server = match self.server_for(&name, config, cwd, &root, output).await {
                Ok(server) => server,
                Err(error) => {
                    results.push(format!("{name}: {error}"));
                    continue;
                }
            };
            let mut server = server.lock().await;
            let position = if matches!(
                args.action.clone(),
                LspAction::Definition
                    | LspAction::References
                    | LspAction::Hover
                    | LspAction::Rename
            ) {
                match position_for(&text, args.line, args.symbol.as_deref()) {
                    Ok(position) => position,
                    Err(error) => return ToolOutcome::failure(error),
                }
            } else {
                json!({"line":0,"character":0})
            };
            let Ok((uri, version, mut notifications)) =
                sync_document(&mut server, &path, &text).await
            else {
                continue;
            };
            if args.action == LspAction::Diagnostics {
                let report =
                    diagnostics_with_fallback(&mut server, &mut notifications, &uri, version).await;
                results.push(report.map_or_else(
                    || format!("{}: no diagnostics received", relative(cwd, &path)),
                    |items| diagnostics_text(Path::new(&relative(cwd, &path)), &items),
                ));
                continue;
            }
            let method = match args.action.clone() {
                LspAction::Definition => "textDocument/definition",
                LspAction::References => "textDocument/references",
                LspAction::Hover => "textDocument/hover",
                LspAction::Symbols => "textDocument/documentSymbol",
                LspAction::Rename => "textDocument/rename",
                LspAction::Diagnostics | LspAction::Status => {
                    return ToolOutcome::failure("invalid LSP action");
                }
            };
            let params = match args.action.clone() {
                LspAction::References => {
                    json!({"textDocument":{"uri":uri},"position":position,"context":{"includeDeclaration":true}})
                }
                LspAction::Rename => {
                    json!({"textDocument":{"uri":uri},"position":position,"newName":args.new_name.as_deref().unwrap_or("")})
                }
                LspAction::Symbols => json!({"textDocument":{"uri":uri}}),
                _ => json!({"textDocument":{"uri":uri},"position":position}),
            };
            let response = tokio::select! {
                _ = cancel.cancelled() => return ToolOutcome::failure("LSP request cancelled"),
                response = server.rpc.request(method, params, Duration::from_secs(20)) => response,
            };
            let value = match response {
                Ok(value) => value,
                Err(error) => {
                    results.push(format!("{name}: {error}"));
                    continue;
                }
            };
            match args.action.clone() {
                LspAction::Definition => results.push(locations_text(cwd, &value, false)),
                LspAction::References => results.push(locations_text(cwd, &value, true)),
                LspAction::Hover => results.push(hover_text(&value)),
                LspAction::Symbols => results.push(symbols_text(&value)),
                LspAction::Rename => {
                    if value.is_null() {
                        results.push("No rename edits available".into());
                        continue;
                    }
                    let preview =
                        workspace_edit_preview(cwd, &value).unwrap_or_else(|error| vec![error]);
                    if args.apply.unwrap_or(true) {
                        match apply_workspace_edit(cwd, &value).await {
                            Ok(changed) => results.push(format!(
                                "Applied rename edits to {} file(s)\n{}",
                                changed.len(),
                                changed.join("\n")
                            )),
                            Err(error) => results.push(format!("Could not apply rename: {error}")),
                        }
                    } else {
                        results.push(if preview.is_empty() {
                            "No edits proposed".into()
                        } else {
                            format!("Rename preview:\n{}", preview.join("\n"))
                        });
                    }
                }
                _ => (),
            }
        }
        if results.is_empty() {
            ToolOutcome::failure("No applicable language server is available")
        } else {
            ToolOutcome::success(results.join("\n"))
        }
    }
}

async fn diagnostics_with_fallback(
    server: &mut RunningServer,
    notifications: &mut tokio::sync::broadcast::Receiver<Value>,
    uri: &str,
    version: u64,
) -> Option<Vec<Value>> {
    let pushed =
        wait_for_push_diagnostics(notifications, uri, version, Duration::from_millis(2500)).await;
    if pushed.is_some() {
        return pushed;
    }
    server.capabilities.get("diagnosticProvider")?;
    let result = server
        .rpc
        .request(
            "textDocument/diagnostic",
            json!({"textDocument":{"uri":uri}}),
            Duration::from_millis(500),
        )
        .await
        .ok()?;
    result.get("items").and_then(Value::as_array).cloned()
}

fn position_for(text: &str, line: Option<u32>, symbol: Option<&str>) -> Result<Value, String> {
    let line = line.unwrap_or(1).saturating_sub(1) as usize;
    let line_text = text
        .lines()
        .nth(line)
        .ok_or_else(|| format!("line {} is outside the file", line + 1))?;
    let character = match symbol {
        Some(symbol) => line_text
            .find(symbol)
            .map(|offset| line_text[..offset].encode_utf16().count())
            .ok_or_else(|| format!("symbol {symbol:?} was not found on line {}", line + 1))?,
        None => 0,
    };
    Ok(json!({"line":line,"character":character}))
}

fn resolve_path(cwd: &Path, file: &str) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}
fn relative(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
