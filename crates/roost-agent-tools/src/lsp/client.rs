//! Ported from oh-my-pi packages/coding-agent/src/lsp/client.ts (MIT).
//! This file owns the Content-Length JSON-RPC transport and response routing.
//! Reader and writer tasks keep notifications flowing while requests are pending.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

type PendingRequests = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
#[derive(Clone)]
pub(super) struct RpcClient {
    outgoing: mpsc::UnboundedSender<Value>,
    pending: PendingRequests,
    next_id: Arc<AtomicU64>,
    notifications: broadcast::Sender<Value>,
    closed: Arc<AtomicBool>,
}

impl RpcClient {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, mut outbound) = mpsc::unbounded_channel::<Value>();
        let (notifications, _) = broadcast::channel(512);
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader_outgoing = outgoing.clone();
        let reader_notifications = notifications.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = closed.clone();
        tokio::spawn(async move {
            let mut reader = reader;
            loop {
                let message = match read_frame(&mut reader).await {
                    Ok(message) => message,
                    Err(error) => {
                        reader_closed.store(true, Ordering::Release);
                        let mut pending = reader_pending.lock().await;
                        for (_, sender) in pending.drain() {
                            let _ = sender.send(Err(error.clone()));
                        }
                        break;
                    }
                };
                if message.get("method").and_then(Value::as_str).is_some() {
                    if let Some(id) = message.get("id") {
                        let response = server_request_response(&message);
                        let _ = reader_outgoing
                            .send(json!({"jsonrpc":"2.0","id":id,"result":response}));
                    } else {
                        let _ = reader_notifications.send(message);
                    }
                } else if let Some(id) = message.get("id").and_then(Value::as_u64)
                    && let Some(sender) = reader_pending.lock().await.remove(&id)
                {
                    let result = if let Some(error) = message.get("error") {
                        Err(format!("LSP response error: {error}"))
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(result);
                }
            }
        });
        let writer_closed = closed.clone();
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(message) = outbound.recv().await {
                if write_frame(&mut writer, &message).await.is_err() {
                    writer_closed.store(true, Ordering::Release);
                    break;
                }
            }
            writer_closed.store(true, Ordering::Release);
        });
        Self {
            outgoing,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            notifications,
            closed,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.outgoing
            .send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .map_err(|error| error.to_string())
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if self
            .outgoing
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err("LSP writer stopped".into());
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("LSP response channel closed".into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(format!("LSP {method} request timed out"))
            }
        }
    }
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value, String> {
    let mut header = Vec::new();
    loop {
        let byte = reader
            .read_u8()
            .await
            .map_err(|error| format!("LSP read header: {error}"))?;
        header.push(byte);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 8192 {
            return Err("LSP header exceeds 8 KiB".into());
        }
    }
    let header = String::from_utf8_lossy(&header);
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| "LSP message has no valid Content-Length".to_owned())?;
    if length > 16 * 1024 * 1024 {
        return Err("LSP message exceeds 16 MiB".into());
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| format!("LSP read body: {error}"))?;
    serde_json::from_slice(&body).map_err(|error| format!("LSP JSON: {error}"))
}

async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|error| format!("LSP write header: {error}"))?;
    writer
        .write_all(&body)
        .await
        .map_err(|error| format!("LSP write body: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("LSP flush: {error}"))
}

fn server_request_response(message: &Value) -> Value {
    match message.get("method").and_then(Value::as_str).unwrap_or("") {
        "workspace/configuration" => message
            .pointer("/params/items")
            .and_then(Value::as_array)
            .map(|items| Value::Array(vec![Value::Null; items.len()]))
            .unwrap_or_else(|| json!([])),
        "workspace/workspaceFolders" => json!([]),
        "workspace/applyEdit" => {
            json!({"applied":false,"failureReason":"workspace edits are not supported by the worker client"})
        }
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => Value::Null,
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    use super::RpcClient;
    use crate::lsp::render::format_diagnostic;

    async fn read_test_frame<R: AsyncRead + Unpin>(
        reader: &mut R,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let mut header = Vec::new();
        loop {
            header.push(reader.read_u8().await?);
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header = String::from_utf8(header)?;
        let length = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: ")?.parse::<usize>().ok())
            .ok_or("missing Content-Length")?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    async fn write_test_frame<W: AsyncWrite + Unpin>(
        writer: &mut W,
        message: &Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::to_vec(message)?;
        writer
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        writer.write_all(&body).await?;
        Ok(())
    }

    #[tokio::test]
    async fn initialize_and_push_notification_are_routed_from_background_reader()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (mut server_read, mut server_write) = tokio::io::split(server_io);
        let fake_server = tokio::spawn(async move {
            let request = read_test_frame(&mut server_read).await?;
            write_test_frame(&mut server_write, &json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/test.rs","diagnostics":[{"range":{"start":{"line":1,"character":2}},"severity":2,"source":"rust-analyzer","message":"unused variable"}]}})).await?;
            write_test_frame(
                &mut server_write,
                &json!({"jsonrpc":"2.0","id":request["id"],"result":{"capabilities":{}}}),
            )
            .await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });
        let client = RpcClient::new(client_read, client_write);
        let mut notifications = client.subscribe();
        let response = client
            .request("initialize", json!({}), Duration::from_secs(1))
            .await
            .map_err(std::io::Error::other)?;
        assert_eq!(response["capabilities"], json!({}));
        let notification =
            tokio::time::timeout(Duration::from_secs(1), notifications.recv()).await??;
        let diagnostic = &notification["params"]["diagnostics"][0];
        assert_eq!(
            format_diagnostic(std::path::Path::new("/tmp/test.rs"), diagnostic),
            "/tmp/test.rs:2:3 [warning] [rust-analyzer] unused variable"
        );
        fake_server.await??;
        Ok(())
    }
}
