//! Ported from oh-my-pi packages/coding-agent/src/lsp/diagnostics.ts (MIT).
//! This file owns didOpen/didChange/didSave versions and fresh diagnostics waits.
//! Notifications are received independently from requests by the JSON-RPC reader.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::types::RunningServer;
use super::uri::file_uri;

pub(super) async fn sync_document(
    server: &mut RunningServer,
    path: &std::path::Path,
    text: &str,
) -> Result<(String, u64, broadcast::Receiver<Value>), String> {
    let uri = file_uri(path)?;
    let mut notifications = server.rpc.subscribe();
    while notifications.try_recv().is_ok() {}
    let version = match server.documents.get(&uri).copied() {
        Some(previous) => {
            let version = previous + 1;
            server.rpc.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]})).await?;
            version
        }
        None => {
            let version = 1;
            let language_id = super::servers::language_id(&server.config, path);
            server.rpc.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":language_id,"version":version,"text":text}})).await?;
            version
        }
    };
    server.documents.insert(uri.clone(), version);
    server
        .rpc
        .notify(
            "textDocument/didSave",
            json!({"textDocument":{"uri":uri},"text":text}),
        )
        .await?;
    server.last_used = Instant::now();
    Ok((uri, version, notifications))
}

pub(super) async fn wait_for_push_diagnostics(
    notifications: &mut broadcast::Receiver<Value>,
    uri: &str,
    version: u64,
    timeout: Duration,
) -> Option<Vec<Value>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, notifications.recv()).await {
            Ok(Ok(message))
                if message.get("method").and_then(Value::as_str)
                    == Some("textDocument/publishDiagnostics") =>
            {
                let params = &message["params"];
                if params.get("uri").and_then(Value::as_str) != Some(uri) {
                    continue;
                }
                if params
                    .get("version")
                    .and_then(Value::as_u64)
                    .is_some_and(|reported| reported < version)
                {
                    continue;
                }
                return params.get("diagnostics").and_then(Value::as_array).cloned();
            }
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{sync_document, wait_for_push_diagnostics};
    use crate::lsp::client::RpcClient;
    use crate::lsp::types::{RunningServer, ServerConfig};

    async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
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

    async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
        writer: &mut W,
        value: &Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::to_vec(value)?;
        writer
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        writer.write_all(&body).await?;
        Ok(())
    }

    #[tokio::test]
    async fn document_versions_increment_and_push_diagnostics_are_fresh()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (mut server_read, mut server_write) = tokio::io::split(server_io);
        let fake_server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..4 {
                let message = read_frame(&mut server_read).await?;
                let method = message["method"].as_str().unwrap_or_default().to_owned();
                if method == "textDocument/didOpen" || method == "textDocument/didChange" {
                    seen.push((
                        method.clone(),
                        message
                            .pointer("/params/textDocument/version")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                    ));
                }
                if method == "textDocument/didSave" {
                    let uri = message
                        .pointer("/params/textDocument/uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let version = seen.last().map(|(_, version)| *version).unwrap_or_default();
                    write_frame(&mut server_write, &json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":version,"diagnostics":[{"range":{"start":{"line":0,"character":0}},"severity":2,"message":"fresh"}]}})).await?;
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(seen)
        });
        let rpc = RpcClient::new(client_read, client_write);
        let mut server = RunningServer {
            rpc,
            child: None,
            capabilities: Value::Null,
            documents: BTreeMap::new(),
            config: ServerConfig {
                command: "fake".into(),
                args: Vec::new(),
                file_types: Vec::new(),
                root_markers: Vec::new(),
                language_ids: BTreeMap::new(),
                init_options: Value::Null,
                settings: Value::Null,
            },
            last_used: Instant::now(),
        };
        let path = std::env::temp_dir().join("document.rs");
        for expected in [1, 2] {
            let (uri, version, mut notifications) =
                sync_document(&mut server, &path, "fn example() {}")
                    .await
                    .map_err(std::io::Error::other)?;
            assert_eq!(version, expected);
            let diagnostics = wait_for_push_diagnostics(
                &mut notifications,
                &uri,
                version,
                std::time::Duration::from_secs(1),
            )
            .await
            .ok_or("diagnostics not received")?;
            assert_eq!(diagnostics[0]["message"], "fresh");
        }
        let seen = fake_server.await??;
        assert_eq!(
            seen,
            vec![
                ("textDocument/didOpen".to_owned(), 1),
                ("textDocument/didChange".to_owned(), 2)
            ]
        );
        Ok(())
    }
}
