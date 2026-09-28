//! `WorkersDeployOutput` through the real coordinator router: a server stream is
//! authenticated at establishment like a unary call, so a caller with no
//! credential is refused before the handler, and a paired browser's stream
//! carries the job's frames to its end.
//!
//! Pins `requireAccountDevice(ctx.values)` of `workersDeployOutput` in
//! `apps/coord/src/deploy/handlers-workers-deploy.ts`, which v2's interceptor
//! ran for every streaming method too.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::net::SocketAddr;

use sync_ws_socket_support::SyncFixture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEPLOY_OUTPUT: &str = "/roost.v1.CoordinatorService/WorkersDeployOutput";

/// One Connect server-streaming JSON call over a raw HTTP/1.1 connection: the
/// status and the de-chunked body, envelopes included.
async fn stream_json(address: SocketAddr, token: Option<&str>, body: &str) -> (u16, String) {
    let mut enveloped = vec![0_u8];
    enveloped.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    enveloped.extend_from_slice(body.as_bytes());
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "POST {DEPLOY_OUTPUT} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{authorization}\
         Content-Type: application/connect+json\r\nConnect-Protocol-Version: 1\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        address.port(),
        enveloped.len()
    );
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&enveloped).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, payload) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, payload.to_owned())
}

#[tokio::test]
async fn a_stream_with_no_credential_is_refused_and_a_browsers_stream_ends_at_done() {
    let stack = SyncFixture::start("deploy-output-http").await;
    let body = r#"{"jobId":"00000000-0000-4000-8000-000000000001"}"#;

    let (_, refused) = stream_json(stack.address, None, body).await;
    assert!(
        refused.contains("unauthenticated"),
        "no credential is refused before the handler: {refused}"
    );
    assert!(!refused.contains("unknown jobId"), "{refused}");

    let (_, device_token) = stack.enroll_browser(11).await;
    let (status, streamed) = stream_json(stack.address, Some(&device_token), body).await;
    assert_eq!(status, 200);
    assert!(
        streamed.contains(r#""kind":"done""#) && streamed.contains("unknown jobId"),
        "the stream carries the job's terminal frame: {streamed}"
    );
}
