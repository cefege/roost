//! Adapter client construction with explicit env-proxy takeover
//! (V14-08 FR-B; `utils/http_proxy` module docs explain the semantics and
//! the reqwest-vs-Node-agents transport note).
//!
//! Every provider-facing `reqwest::Client` in the adapter layer is built
//! through [`adapter_client_builder`]: reqwest's built-in env proxying is
//! disabled first (`.no_proxy()` — its suffix-matching `NO_PROXY` semantics
//! are exactly the pre-#8737 bug), then a proxy resolved for the target URL
//! with the upstream `shouldProxyHostname` semantics is injected, gated on
//! the target scheme (upstream picks `HttpProxyAgent` vs `HttpsProxyAgent`
//! by target protocol).

use crate::types::ProviderEnv;
use crate::utils::http_proxy::resolve_http_proxy_for_target;

/// Build a client builder with the env proxy decision taken over for
/// `target_url`. `Ok(builder)` always produces a usable builder — a direct
/// connection when no proxy resolves; `Err` mirrors the upstream
/// `resolveHttpProxyUrlForTarget` failures (invalid / SOCKS-PAC proxy URL).
pub fn adapter_client_builder(
    env: Option<&ProviderEnv>,
    target_url: &str,
) -> Result<reqwest::ClientBuilder, String> {
    // Disable reqwest's env/system proxy first: `no_proxy()` clears any
    // proxies and turns off auto detection; the explicit proxy below (if
    // any) is added afterwards and wins.
    let mut builder = reqwest::Client::builder().no_proxy();
    let target_is_http = url::Url::parse(target_url)
        .map(|parsed| parsed.scheme() == "http")
        .unwrap_or(false);
    if let Some(proxy_url) = resolve_http_proxy_for_target(target_url, env)? {
        let proxy = if target_is_http {
            reqwest::Proxy::http(proxy_url)
        } else {
            reqwest::Proxy::https(proxy_url)
        }
        .map_err(|error| format!("Invalid proxy URL: {error}"))?;
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No env → no proxy entry → plain builder (direct); scheme-gated
    /// injection for a resolved proxy.
    #[test]
    fn builder_direct_without_env() {
        let env: ProviderEnv = [(
            "HTTPS_PROXY".to_owned(),
            "http://proxy.example:8080".to_owned(),
        )]
        .into_iter()
        .collect();
        // NO_PROXY exempts the target → builder builds fine (direct).
        let mut env_with_exempt = env.clone();
        env_with_exempt.insert("NO_PROXY".to_owned(), "api.example.com".to_owned());
        let builder = adapter_client_builder(Some(&env_with_exempt), "https://api.example.com")
            .expect("builder");
        builder.build().expect("direct client builds");

        // A SOCKS proxy for a covered target surfaces the upstream error.
        let socks_env: ProviderEnv = [(
            "HTTPS_PROXY".to_owned(),
            "socks5://proxy.example:1080".to_owned(),
        )]
        .into_iter()
        .collect();
        let error = adapter_client_builder(Some(&socks_env), "https://api.example.com")
            .expect_err("socks rejected");
        assert!(error.starts_with(crate::utils::http_proxy::UNSUPPORTED_PROXY_PROTOCOL_MESSAGE));
    }

    /// V14-13 FR-B 复现记录（D-094）：reqwest 对**纯 HTTP origin** 的代理
    /// 请求使用 absolute-URI 转发（`GET http://host/... HTTP/1.1`），而非
    /// 上游 `proxyTunnel: true` 的 CONNECT 隧道（http-dispatcher.ts:86-90 @
    /// `23842b1e6`，#8134）。reqwest 0.12 无强制 HTTP-over-CONNECT 开关；
    /// 该差异触发条件 = 「配置代理 + http:// provider base_url」。本测试
    /// 钉死当前转发形状，D-094 落盘于 plan/v0.1.4/deviations/。
    #[tokio::test]
    async fn http_origin_uses_absolute_uri_forwarding_not_connect() {
        use std::sync::Arc;

        // A fake origin (never reached directly) and a recording proxy.
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind origin");
        let origin_addr = origin.local_addr().expect("origin addr");
        drop(origin); // nothing listens: any non-proxied attempt fails

        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy");
        let proxy_addr = proxy.local_addr().expect("proxy addr");
        let first_line: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let recorder = first_line.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut socket, _)) = proxy.accept().await {
                let recorder = recorder.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let line = String::from_utf8_lossy(&head)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    *recorder.lock().unwrap() = Some(line);
                    let body = r#"{"proxied":true}"#;
                    let out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        let env: ProviderEnv = [("HTTP_PROXY".to_owned(), format!("http://{proxy_addr}"))]
            .into_iter()
            .collect();
        let builder =
            adapter_client_builder(Some(&env), &format!("http://{origin_addr}")).expect("builder");
        let client = builder.build().expect("client");

        let response = client
            .get(format!("http://{origin_addr}/v1/models"))
            .send()
            .await
            .expect("proxied response");
        assert!(response.status().is_success());

        let line = first_line.lock().unwrap().clone().expect("request seen");
        assert!(
            line.starts_with(&format!("GET http://{origin_addr}/")),
            "absolute-URI forwarding (D-094): {line:?}"
        );
        assert!(!line.starts_with("CONNECT "), "no CONNECT tunnel: {line:?}");
    }
}
