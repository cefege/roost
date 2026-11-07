//! Port of `packages/agent/src/harness/tools/file-mutation-queue.ts` @ pi
//! 0.82.1 (2efa728) — serialize file mutations targeting the same environment
//! and canonical path.
//!
//! Intentional differences:
//! - The upstream `WeakMap<ExecutionEnv, MutationQueueState>`
//!   (file-mutation-queue.ts:9) becomes a global registry keyed by the env's
//!   address: trait objects have no WeakMap, and the callers pass `&dyn
//!   ExecutionEnv` (never the owning `Arc` — see the write/edit tool call
//!   sites), so a `Weak` key cannot be taken. Each entry is leased by an
//!   RAII guard for as long as any operation holds it and is removed the
//!   moment the last operation settles, so the registry tracks live
//!   operations only: a dropped env with no in-flight operation leaves
//!   nothing behind, and an env dropped mid-operation keeps its entry until
//!   that operation settles. The one residual divergence from a WeakMap is
//!   env-address reuse while an old env's operation is still in flight: the
//!   new env then shares the old entry (and any per-path queue) until it
//!   settles, where a WeakMap would start a fresh state.
//! - The upstream registration chain (file-mutation-queue.ts:31-46) is
//!   dropped: it only serializes `getMutationQueueKey`, whose two calls
//!   (`absolutePath` — pure string normalization — and `canonicalPath` —
//!   realpath) have no observable interleaving effects; per-key FIFO ordering
//!   is preserved by the fair `tokio::sync::Mutex` (tokio mutexes are FIFO).
//! - `getOrThrow` / thrown `FileError`s become `AgentError::Message` with the
//!   upstream message text.
//! - `Promise<void>` queues become `Arc<tokio::sync::Mutex<()>>`; the queue
//!   entry GC uses the Arc strong count (2 = map + our clone) instead of
//!   upstream's `queues.get(key) === chainedQueue` identity check — the two
//!   are equivalent under the map lock.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::AgentError;
use crate::harness::types::{ExecutionEnv, FileErrorCode};

/// `MutationQueueState` (file-mutation-queue.ts:4-8) — per-env state.
struct MutationQueueState {
    /// `queues` — per-canonical-path FIFO locks (the `Promise<void>` chain of
    /// file-mutation-queue.ts:5).
    queues: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Operations currently holding this state (registration through the
    /// final GC). A non-zero count keeps the registry entry alive, so one
    /// operation's entry GC cannot strand a concurrent registration.
    in_flight: AtomicUsize,
}

/// Per-env states keyed by env address (see the module header). An entry
/// exists only while at least one operation holds it; the last holder's
/// [`StateGuard`] drops it, so entries never outlive the operations (no
/// leak — the former `Box::leak` kept one entry per env address forever).
static STATES: OnceLock<Mutex<HashMap<usize, Arc<MutationQueueState>>>> = OnceLock::new();

/// Registry key: the env's address (see the module header).
fn env_address(env: &dyn ExecutionEnv) -> usize {
    (env as *const dyn ExecutionEnv) as *const () as usize
}

/// `getState` (file-mutation-queue.ts:11-18) — keyed by the env's address
/// (computed by the caller). The in-flight count is bumped under the registry
/// lock, so the entry cannot be removed between the get-or-create and the
/// bump.
fn get_state(key: usize) -> Arc<MutationQueueState> {
    let mut states = STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = states
        .entry(key)
        .or_insert_with(|| {
            Arc::new(MutationQueueState {
                queues: Mutex::new(HashMap::new()),
                in_flight: AtomicUsize::new(0),
            })
        })
        .clone();
    state.in_flight.fetch_add(1, Ordering::SeqCst);
    state
}

/// Per-env registry lease (the WeakMap entry dies with its env): releases the
/// state and removes the registry entry once no operation holds it any more.
struct StateGuard {
    state: Arc<MutationQueueState>,
    key: usize,
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        let mut states = STATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = states.get(&self.key) {
            // `Arc::ptr_eq` guards against address reuse: if the address has
            // been taken over by a newer env's state, that entry stays.
            if Arc::ptr_eq(entry, &self.state) && self.state.in_flight.load(Ordering::SeqCst) == 0 {
                states.remove(&self.key);
            }
        }
    }
}

/// `getMutationQueueKey` (file-mutation-queue.ts:20-26): canonicalize, falling
/// back to the absolute path when the path does not exist or the backend does
/// not support canonicalization; other errors propagate.
async fn get_mutation_queue_key(env: &dyn ExecutionEnv, path: &str) -> Result<String, AgentError> {
    let absolute_path = env
        .absolute_path(path, None)
        .await
        .map_err(|error| AgentError::Message(error.message))?;
    match env.canonical_path(&absolute_path, None).await {
        Ok(canonical_path) => Ok(canonical_path),
        Err(error)
            if error.code == FileErrorCode::NotFound
                || error.code == FileErrorCode::NotSupported =>
        {
            Ok(absolute_path)
        }
        Err(error) => Err(AgentError::Message(error.message)),
    }
}

/// `withFileMutationQueue` (file-mutation-queue.ts:29-55) — run `f` while
/// holding the per-env, per-canonical-path queue. The queue is released in a
/// `finally`-equivalent, so an aborted or failed mutation still unlocks the
/// path for later operations.
pub async fn with_file_mutation_queue<T, Fut>(
    env: &dyn ExecutionEnv,
    path: &str,
    f: Fut,
) -> Result<T, AgentError>
where
    Fut: Future<Output = Result<T, AgentError>>,
{
    let key = env_address(env);
    let state = get_state(key);
    // The lease runs to the end of this scope — including aborts, where the
    // dropped future releases the entry (any queue Arc registered earlier is
    // dropped with the state).
    let _state_guard = StateGuard {
        state: Arc::clone(&state),
        key,
    };
    let queue_key = get_mutation_queue_key(env, path).await?;

    // Registration phase: get-or-create the per-key mutex, clone the Arc, then
    // release the state lock (file-mutation-queue.ts:33-41).
    let queue = {
        let mut queues = state
            .queues
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queues
            .entry(queue_key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Acquire the per-key lock — serializes concurrent mutations of the same
    // file (including symlink aliases). The guard lives across `f`, so an
    // aborted operation holds the queue until it settles (the abort checks
    // inside the tool closures release it via this scope).
    let result = {
        let _guard = queue.lock().await;
        f.await
    };

    // GC phase: remove the entry if we are the last holder besides the map
    // (file-mutation-queue.ts:54). While holding the state lock no new
    // registrations can occur, so the strong-count check is race-free.
    {
        let mut queues = state
            .queues
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = queues.get(&queue_key)
            && Arc::strong_count(entry) == 2
        {
            queues.remove(&queue_key);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::tools::test_helpers::TempDir;

    fn registry() -> &'static Mutex<HashMap<usize, Arc<MutationQueueState>>> {
        STATES
            .get()
            .expect("registry must be initialized by the operations under test")
    }

    #[tokio::test]
    async fn registry_entries_are_reclaimed_once_operations_settle() {
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let address = env_address(env.as_ref());

        // An entry exists while an operation is in flight, and both
        // concurrent operations share the single per-env state.
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let op = tokio::spawn({
            let env = Arc::clone(&env);
            async move {
                with_file_mutation_queue(env.as_ref(), "a.txt", async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    Ok(())
                })
                .await
            }
        });
        entered_rx.await.expect("op must enter the queue section");
        let entry = {
            let states = registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            states
                .get(&address)
                .cloned()
                .expect("an in-flight operation must hold a registry entry")
        };
        assert_eq!(entry.in_flight.load(Ordering::SeqCst), 1);

        // Dropping the env mid-operation must not leave the entry behind
        // once the operation settles (the guard holds the state, not the
        // env).
        drop(env);
        release_tx.send(()).expect("release the queued operation");
        op.await
            .expect("operation task must not panic")
            .expect("operation must succeed");
        let states = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            states.get(&address).is_none(),
            "a settled operation must release its registry entry"
        );
    }

    #[tokio::test]
    async fn sequential_operations_release_the_registry_entry() {
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let address = env_address(env.as_ref());

        with_file_mutation_queue(env.as_ref(), "a.txt", async { Ok(()) })
            .await
            .unwrap();
        // The entry was removed when the operation settled — a dropped env
        // with no in-flight operation leaves nothing behind.
        drop(env);
        let states = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            states.get(&address).is_none(),
            "a settled operation must not leave a registry entry behind"
        );
    }
}
