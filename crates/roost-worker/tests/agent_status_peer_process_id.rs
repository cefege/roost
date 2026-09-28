//! The kernel peer-process reader behind the agent report socket: injected
//! queries pin the fail-closed validation, and a real child connection proves
//! the platform query end to end. Ports v2
//! `apps/worker/tests/agents/agent-status-peer-process-id.test.ts`; its
//! named-pipe case is Windows-only and not ported (Windows is paused).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_worker::agents::peer_process_id::{LocalPeerProcessIdReader, NativePeerProcessIdQuery};
use tokio::net::{UnixListener, UnixStream};

/// What the injected query answers next: a pid, v2's `null`, or a throw.
#[derive(Default)]
struct ScriptedQuery {
    answer: Mutex<Option<Result<Option<i64>, String>>>,
    calls: AtomicUsize,
    closed: AtomicBool,
}

impl ScriptedQuery {
    fn answering(answer: Result<Option<i64>, String>) -> Arc<Self> {
        let query = Arc::new(Self::default());
        *query.answer.lock().unwrap() = Some(answer);
        query
    }
    fn set(&self, answer: Result<Option<i64>, String>) {
        *self.answer.lock().unwrap() = Some(answer);
    }
}

impl NativePeerProcessIdQuery for ScriptedQuery {
    fn read(&self, _: &UnixStream) -> Result<Option<i64>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.lock().unwrap().clone().unwrap()
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

fn scratch_socket(name: &str) -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("roost-peer-pid-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("peer.sock")
}

/// An accepted server-side socket connected from this process.
async fn accepted_pair(name: &str) -> (UnixStream, UnixStream) {
    let path = scratch_socket(name);
    let listener = UnixListener::bind(&path).unwrap();
    let client = UnixStream::connect(&path).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (server, client)
}

#[tokio::test]
async fn passes_the_accepted_socket_to_the_native_peer_query() {
    let query = ScriptedQuery::answering(Ok(Some(4_321)));
    let reader = LocalPeerProcessIdReader::with_query(
        Arc::clone(&query) as Arc<dyn NativePeerProcessIdQuery>
    );
    let (server, _client) = accepted_pair("passes").await;
    assert_eq!(reader.read(&server), Some(4_321));
    assert_eq!(query.calls.load(Ordering::SeqCst), 1);
    reader.close();
    assert!(query.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn fails_closed_on_invalid_native_results_and_query_failures() {
    let query = ScriptedQuery::answering(Ok(Some(0)));
    let reader = LocalPeerProcessIdReader::with_query(
        Arc::clone(&query) as Arc<dyn NativePeerProcessIdQuery>
    );
    let (server, _client) = accepted_pair("fails").await;
    assert_eq!(reader.read(&server), None);
    query.set(Ok(Some(-1)));
    assert_eq!(reader.read(&server), None);
    query.set(Ok(None));
    assert_eq!(reader.read(&server), None);
    query.set(Ok(Some(i64::from(u32::MAX) + 1)));
    assert_eq!(reader.read(&server), None);
    query.set(Ok(Some(4_567)));
    assert_eq!(reader.read(&server), Some(4_567));
    query.set(Err("native query failed".to_owned()));
    assert_eq!(reader.read(&server), None);
}

#[tokio::test]
async fn reads_the_actual_pid_of_a_separate_local_socket_client() {
    let path = scratch_socket("child");
    let listener = UnixListener::bind(&path).unwrap();
    let reader = LocalPeerProcessIdReader::native();
    let connect = "import os, socket\ns = socket.socket(socket.AF_UNIX)\ns.connect(os.environ['ROOST_TEST_ENDPOINT'])\ns.recv(1)\n";
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", connect])
        .env("ROOST_TEST_ENDPOINT", &path)
        .kill_on_drop(true)
        .spawn()
        .expect("python3 connects a separate client process");
    let child_pid = child.id().unwrap();
    let (server, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let peer_pid = reader.read(&server);
    drop(server);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert_eq!(peer_pid, Some(child_pid));
}
