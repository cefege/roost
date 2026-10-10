//! Ported from oh-my-pi packages/coding-agent/src/lsp/client.ts (MIT).
//! This file owns per-worker server slots, lazy process startup and lifecycle.
//! A background reaper enforces ten-minute idle shutdown and the eight-process cap.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::{Mutex, OnceCell, mpsc};

use super::client::RpcClient;
use super::documents::{sync_document, wait_for_push_diagnostics};
use super::install::Installer;
use super::render::format_diagnostic;
use super::servers::{configured_servers, selected_servers};
use super::types::{RunningServer, ServerConfig, ServerSlot};
use crate::search_path::resolve_binary;

const IDLE_LIMIT_MS: u64 = 10 * 60 * 1000;
const SERVER_LIMIT: usize = 8;

#[derive(Clone)]
pub struct LspManager {
    pub(super) data_dir: PathBuf,
    pub(super) servers: Arc<Mutex<HashMap<String, Arc<ServerSlot>>>>,
    installer: Installer,
}

impl fmt::Debug for LspManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LspManager")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl LspManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let servers = Arc::new(Mutex::new(HashMap::new()));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&servers);
            runtime.spawn(async move {
                idle_reaper(weak).await;
            });
        }
        Self {
            installer: Installer::new(data_dir.clone(), None),
            data_dir,
            servers,
        }
    }

    pub async fn diagnostics_after_write(
        &self,
        cwd: &Path,
        file: &Path,
        text: &str,
        out: &mpsc::Sender<String>,
    ) -> Option<String> {
        let selected = selected_servers(cwd, file);
        if selected.is_empty() {
            return None;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut diagnostics = Vec::new();
        let mut received = false;
        for (name, config, root) in selected {
            let server = match self.server_for(&name, config, cwd, &root, out).await {
                Ok(server) => server,
                Err(error) => {
                    let _ = out.send(error).await;
                    continue;
                }
            };
            let mut server = server.lock().await;
            let Ok((uri, version, mut notifications)) =
                sync_document(&mut server, file, text).await
            else {
                continue;
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let push_wait = remaining.min(Duration::from_millis(2500));
            if let Some(items) =
                wait_for_push_diagnostics(&mut notifications, &uri, version, push_wait).await
            {
                received = true;
                diagnostics.extend(items);
            } else if server.capabilities.get("diagnosticProvider").is_some() {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if !remaining.is_zero()
                    && let Ok(report) = server
                        .rpc
                        .request(
                            "textDocument/diagnostic",
                            json!({"textDocument":{"uri":uri}}),
                            remaining,
                        )
                        .await
                    && let Some(items) = report.get("items").and_then(Value::as_array)
                {
                    received = true;
                    diagnostics.extend(items.iter().cloned());
                }
            }
            server.last_used = Instant::now();
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        if !received {
            return Some("No diagnostics received within 3 seconds".into());
        }
        let relative = file.strip_prefix(cwd).unwrap_or(file);
        Some(render_diagnostic_summary(relative, &diagnostics))
    }

    pub async fn shutdown_all(&self) {
        let slots = {
            let mut servers = self.servers.lock().await;
            servers.drain().map(|(_, slot)| slot).collect::<Vec<_>>()
        };
        for slot in slots {
            stop_slot(&slot).await;
        }
    }

    pub async fn evict_idle(&self) {
        evict_idle_slots(&self.servers).await;
    }

    pub(super) async fn server_for(
        &self,
        name: &str,
        config: ServerConfig,
        cwd: &Path,
        root: &Path,
        output: &mpsc::Sender<String>,
    ) -> Result<Arc<Mutex<RunningServer>>, String> {
        let key = format!("{name}:{}", root.display());
        let (slot, evicted) = {
            let mut servers = self.servers.lock().await;
            let now = now_ms();
            let stale_keys = servers
                .iter()
                .filter(|(_, slot)| {
                    now.saturating_sub(slot.last_used_ms.load(Ordering::Relaxed)) >= IDLE_LIMIT_MS
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            let mut evicted = stale_keys
                .into_iter()
                .filter_map(|key| servers.remove(&key))
                .collect::<Vec<_>>();
            let slot = if let Some(slot) = servers.get(&key) {
                slot.clone()
            } else {
                if servers.len() >= SERVER_LIMIT
                    && let Some(oldest) = servers
                        .iter()
                        .min_by_key(|(_, slot)| slot.last_used_ms.load(Ordering::Relaxed))
                        .map(|(key, _)| key.clone())
                    && let Some(slot) = servers.remove(&oldest)
                {
                    evicted.push(slot);
                }
                let slot = Arc::new(ServerSlot {
                    server: OnceCell::new(),
                    last_used_ms: now.into(),
                    name: name.to_owned(),
                    root: root.to_path_buf(),
                });
                servers.insert(key.clone(), slot.clone());
                slot
            };
            (slot, evicted)
        };
        for stale in evicted {
            stop_slot(&stale).await;
        }
        let running = initialize_slot(self, slot.clone(), name, config, cwd, root, output).await?;
        if running.lock().await.rpc.is_closed() {
            let dead = {
                let mut servers = self.servers.lock().await;
                if servers
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &slot))
                {
                    servers.remove(&key)
                } else {
                    None
                }
            };
            if let Some(dead) = dead {
                stop_slot(&dead).await;
            }
            return Err(format!("{name} disconnected"));
        }
        Ok(running)
    }

    async fn start_server(
        &self,
        name: &str,
        config: ServerConfig,
        cwd: &Path,
        root: &Path,
        output: &mpsc::Sender<String>,
    ) -> Result<RunningServer, String> {
        let binary = match resolve_binary(&config.command, cwd, &self.data_dir) {
            Some(binary) => binary,
            None if matches!(name, "rust-analyzer" | "ruff" | "biome") => self
                .installer
                .install(name, output)
                .await
                .map_err(|error| format!("failed to install {name}: {error}"))?,
            None => return Err(format!("{} is not installed", config.command)),
        };
        let mut child = Command::new(binary)
            .args(
                config
                    .args
                    .iter()
                    .map(|arg| arg.replace("$PID", &std::process::id().to_string())),
            )
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "LSP stdin unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "LSP stdout unavailable".to_owned())?;
        let rpc = RpcClient::new(stdout, stdin);
        let root_uri = super::uri::file_uri(root)?;
        let initialize = json!({"processId":std::process::id(),"rootUri":root_uri,"capabilities":{"workspace":{"configuration":true,"workspaceFolders":true},"textDocument":{"publishDiagnostics":{"relatedInformation":true},"hover":{"contentFormat":["markdown","plaintext"]}}},"initializationOptions":config.init_options,"workspaceFolders":[{"uri":root_uri,"name":root.file_name().and_then(|value| value.to_str()).unwrap_or("workspace")}]});
        let response = rpc
            .request("initialize", initialize, Duration::from_secs(20))
            .await?;
        rpc.notify("initialized", json!({})).await?;
        rpc.notify(
            "workspace/didChangeConfiguration",
            json!({"settings":config.settings}),
        )
        .await?;
        tracing::info!(server = %name, root = %root.display(), "language server started");
        Ok(RunningServer {
            rpc,
            child: Some(child),
            capabilities: response.get("capabilities").cloned().unwrap_or(Value::Null),
            documents: Default::default(),
            config,
            last_used: Instant::now(),
        })
    }

    pub(super) async fn status_text(&self, cwd: &Path) -> String {
        let servers = self.servers.lock().await;
        let mut lines = Vec::new();
        for slot in servers.values() {
            if slot.server.get().is_some() {
                let idle =
                    now_ms().saturating_sub(slot.last_used_ms.load(Ordering::Relaxed)) / 1000;
                lines.push(format!(
                    "{}: running at {} (idle {idle}s)",
                    slot.name,
                    slot.root.display()
                ));
            }
        }
        lines.extend(configured_servers(cwd, &self.data_dir));
        if lines.is_empty() {
            "No language servers configured or running".into()
        } else {
            lines.join("\n")
        }
    }
}

async fn initialize_slot(
    manager: &LspManager,
    slot: Arc<ServerSlot>,
    name: &str,
    config: ServerConfig,
    cwd: &Path,
    root: &Path,
    output: &mpsc::Sender<String>,
) -> Result<Arc<Mutex<RunningServer>>, String> {
    let server = slot
        .server
        .get_or_try_init(|| async {
            manager
                .start_server(name, config, cwd, root, output)
                .await
                .map(|server| Arc::new(Mutex::new(server)))
        })
        .await
        .map_err(|error| error.to_owned())?
        .clone();
    slot.last_used_ms.store(now_ms(), Ordering::Relaxed);
    Ok(server)
}

async fn idle_reaper(servers: std::sync::Weak<Mutex<HashMap<String, Arc<ServerSlot>>>>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let Some(servers) = servers.upgrade() else {
            break;
        };
        evict_idle_slots(&servers).await;
    }
}

async fn evict_idle_slots(servers: &Arc<Mutex<HashMap<String, Arc<ServerSlot>>>>) {
    let expired = {
        let mut servers = servers.lock().await;
        let now = now_ms();
        let keys = servers
            .iter()
            .filter(|(_, slot)| {
                now.saturating_sub(slot.last_used_ms.load(Ordering::Relaxed)) >= IDLE_LIMIT_MS
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| servers.remove(&key))
            .collect::<Vec<_>>()
    };
    for slot in expired {
        stop_slot(&slot).await;
    }
}

async fn stop_slot(slot: &ServerSlot) {
    if let Some(server) = slot.server.get() {
        let mut server = server.lock().await;
        let _ = server
            .rpc
            .request("shutdown", Value::Null, Duration::from_secs(1))
            .await;
        let _ = server.rpc.notify("exit", Value::Null).await;
        if let Some(child) = &mut server.child {
            let _ = child.kill().await;
        }
        tracing::info!(server = %slot.name, root = %slot.root.display(), "language server stopped");
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn render_diagnostic_summary(path: &Path, diagnostics: &[Value]) -> String {
    if diagnostics.is_empty() {
        return "0 error(s), 0 warning(s)".into();
    }
    let errors = diagnostics
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_u64).unwrap_or(1) == 1)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_u64) == Some(2))
        .count();
    let rendered = diagnostics
        .iter()
        .map(|item| format_diagnostic(path, item))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{rendered}\n{errors} error(s), {warnings} warning(s)")
}
