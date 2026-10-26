//! Per-file mutation serialization (port of
//! `packages/durable/src/tools/file-mutation-queue.ts` @ `abe508e1b`).

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use crate::chord::context::Context;
use crate::env::types::ExecutionEnv;
use crate::errors::FileErrorCode;
use crate::harness::types::ToolExecuteError;

/// Tail of the mutation chain of each file, keyed by file system id and
/// canonical path.
static QUEUES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn mutation_key(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &Context,
) -> Result<String, ToolExecuteError> {
    let absolute = env.absolute_path(path, context).await?;
    Ok(format!(
        "{}\0{}",
        env.id(),
        canonical(env, &absolute, context).await?
    ))
}

/// The canonical path; for a file that does not exist yet, its canonical
/// parent joined with its name, so a `write` that creates a file and a later
/// mutation of it share one key even under a symlinked directory.
fn canonical<'a>(
    env: &'a dyn ExecutionEnv,
    absolute: &'a str,
    context: &'a Context,
) -> futures::future::BoxFuture<'a, Result<String, ToolExecuteError>> {
    Box::pin(async move {
        match env.canonical_path(absolute, context).await {
            Ok(path) => Ok(path),
            Err(error) if error.code == FileErrorCode::NotSupported => Ok(absolute.to_string()),
            Err(error) if error.code == FileErrorCode::NotFound => {
                let Some(parent) = Path::new(absolute).parent() else {
                    return Ok(absolute.to_string());
                };
                let parent = parent.to_string_lossy().into_owned();
                if parent.is_empty() || parent == absolute {
                    return Ok(absolute.to_string());
                }
                let name = Path::new(absolute)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let canonical_parent = canonical(env, &parent, context).await?;
                Ok(std::path::Path::new(&canonical_parent)
                    .join(name)
                    .to_string_lossy()
                    .into_owned())
            }
            Err(error) => Err(error.into()),
        }
    })
}

/// Serialize `edit` and `write` mutations of one file within this process:
/// same file system id and canonical path, whichever environment object the
/// call got. Other files, and other file systems, never wait. Concurrent calls
/// on one file run in the order their keys resolve. Not a lock against `bash`
/// or other processes.
pub async fn with_file_mutation_queue<T, F>(
    env: Arc<dyn ExecutionEnv>,
    path: &str,
    context: &Context,
    run: F,
) -> Result<T, ToolExecuteError>
where
    F: Future<Output = Result<T, ToolExecuteError>>,
{
    let key = mutation_key(env.as_ref(), path, context).await?;
    // Take the slot without awaiting, so no other call can take it in between.
    let queue = {
        let mut queues = QUEUES.lock().expect("mutation queues");
        Arc::clone(
            queues
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let guard = queue.lock().await;
    let result = run.await;
    drop(guard);
    {
        let mut queues = QUEUES.lock().expect("mutation queues");
        if let Some(current) = queues.get(&key)
            && Arc::ptr_eq(current, &queue)
            && Arc::strong_count(current) <= 2
        {
            queues.remove(&key);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
    use crate::harness::types::ToolExecuteError;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn env_for(dir: &std::path::Path) -> Arc<dyn ExecutionEnv> {
        Arc::new(NodeExecutionEnv::new(NodeExecutionEnvOptions {
            cwd: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        }))
    }

    fn context() -> Context {
        crate::chord::context::BACKGROUND_CONTEXT.clone()
    }

    #[tokio::test]
    async fn serializes_mutations_of_one_file() {
        let dir = temp_dir("mutation-queue-one");
        let env = env_for(&dir);
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let gate = Arc::new(tokio::sync::Notify::new());
        let path = dir.join("f.txt").to_string_lossy().into_owned();

        let first = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let gate = Arc::clone(&gate);
            let path = path.clone();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("first-start");
                    gate.notified().await;
                    order.lock().expect("order").push("first-end");
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let second = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let path = path.clone();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("second-start");
                    order.lock().expect("order").push("second-end");
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(&*order.lock().expect("order"), &["first-start"]);
        gate.notify_one();
        first.await.expect("join").expect("first");
        second.await.expect("join").expect("second");
        assert_eq!(
            &*order.lock().expect("order"),
            &["first-start", "first-end", "second-start", "second-end"]
        );
        // Other tests may hold unrelated keys in the process-global map, so
        // only this file's key must be gone.
        let key = mutation_key(env.as_ref(), &path, &context())
            .await
            .expect("key");
        assert!(!QUEUES.lock().expect("queues").contains_key(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn different_files_do_not_wait_for_each_other() {
        let dir = temp_dir("mutation-queue-two");
        let env = env_for(&dir);
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let gate = Arc::new(tokio::sync::Notify::new());

        let first = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let gate = Arc::clone(&gate);
            let path = dir.join("a.txt").to_string_lossy().into_owned();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("a-start");
                    gate.notified().await;
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let second = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let path = dir.join("b.txt").to_string_lossy().into_owned();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("b-done");
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .expect("a different file never waits")
            .expect("join")
            .expect("second");
        assert_eq!(&*order.lock().expect("order"), &["a-start", "b-done"]);
        gate.notify_one();
        first.await.expect("join").expect("first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_keeps_one_key_for_a_new_file() {
        let dir = temp_dir("mutation-queue-link");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).expect("real dir");
        std::os::unix::fs::symlink(&real, dir.join("alias")).expect("symlink");
        let env = env_for(&dir);
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let gate = Arc::new(tokio::sync::Notify::new());

        // The not-yet-existing file under the real path and under the symlinked
        // directory resolve to one canonical key.
        let first = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let gate = Arc::clone(&gate);
            let path = real.join("new.txt").to_string_lossy().into_owned();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("first-start");
                    gate.notified().await;
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let second = {
            let env = Arc::clone(&env);
            let order = Arc::clone(&order);
            let path = dir.join("alias/new.txt").to_string_lossy().into_owned();
            tokio::spawn(async move {
                with_file_mutation_queue(env, &path, &context(), async {
                    order.lock().expect("order").push("second-start");
                    Ok::<(), ToolExecuteError>(())
                })
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(&*order.lock().expect("order"), &["first-start"]);
        gate.notify_one();
        first.await.expect("join").expect("first");
        second.await.expect("join").expect("second");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
