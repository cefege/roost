//! Execution-environment conformance tests (port of
//! `packages/durable/src/testing/env-conformance.ts` @ `abe508e1b` against
//! [`NodeExecutionEnv`]).

#[path = "tools_support.rs"]
mod tools_support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context, with_abort_signal};
use rpi_durable::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
use rpi_durable::env::types::{
    CreateDirOptions, DirPage, ExecutionErrorCode, FileContent, FileSystem, RemoveOptions,
    ScanLinesOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult, ShellOutputInfo,
    ShellOutputWindow, ShellStream, WatchChange, WatchTarget,
};
use rpi_durable::errors::{FileError, FileErrorCode};
use tokio_util::sync::CancellationToken;
use tools_support::TempDir;

fn context() -> Context {
    BACKGROUND_CONTEXT.clone()
}

fn aborted_context() -> Context {
    let token = CancellationToken::new();
    token.cancel();
    with_abort_signal(token, &BACKGROUND_CONTEXT)
}

fn create_env(dir: &TempDir) -> Arc<NodeExecutionEnv> {
    Arc::new(NodeExecutionEnv::new(NodeExecutionEnvOptions {
        cwd: Some(dir.path_str()),
        ..Default::default()
    }))
}

fn code_text(code: FileErrorCode) -> &'static str {
    match code {
        FileErrorCode::Aborted => "aborted",
        FileErrorCode::NotFound => "not_found",
        FileErrorCode::PermissionDenied => "permission_denied",
        FileErrorCode::NotDirectory => "not_directory",
        FileErrorCode::IsDirectory => "is_directory",
        FileErrorCode::Invalid => "invalid",
        FileErrorCode::NotSupported => "not_supported",
        FileErrorCode::Unknown => "unknown",
    }
}

fn error_code<T>(result: &Result<T, FileError>) -> Option<&'static str> {
    result.as_ref().err().map(|error| code_text(error.code))
}

fn write(env: &NodeExecutionEnv, path: &str, content: impl Into<FileContent>) {
    futures::executor::block_on(env.write_file(path, content.into(), &context())).expect("write");
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binary_reader_reads_byte_ranges_of_the_opened_file() {
    let dir = TempDir::new("env-binary-read");
    let env = create_env(&dir);
    write(&env, "data.txt", "hello world");
    let reader = env
        .open_binary_reader("data.txt", None, &context())
        .await
        .expect("reader");
    let info = reader.info(&context()).await.expect("info");
    assert_eq!(info.name, "data.txt");
    assert_eq!(info.kind, rpi_durable::env::types::FileKind::File);
    assert_eq!(info.size, 11);
    assert_eq!(reader.read(0, 5, &context()).await.expect("read"), b"hello");
    assert_eq!(
        reader.read(6, 100, &context()).await.expect("read"),
        b"world"
    );
    assert!(
        reader
            .read(11, 4, &context())
            .await
            .expect("read")
            .is_empty()
    );
    assert!(
        reader
            .read(50, 1, &context())
            .await
            .expect("read")
            .is_empty()
    );
    assert!(
        reader
            .read(3, 0, &context())
            .await
            .expect("read")
            .is_empty()
    );
    assert_eq!(
        error_code(&reader.read(u64::MAX, 1, &context()).await),
        Some("invalid")
    );
    assert_eq!(
        error_code(&reader.read(0, 1, &aborted_context()).await),
        Some("aborted")
    );
    reader.close(&context()).await;
    reader.close(&context()).await;
    assert_eq!(
        error_code(&reader.read(0, 1, &context()).await),
        Some("invalid")
    );
    assert_eq!(error_code(&reader.info(&context()).await), Some("invalid"));
}

#[tokio::test]
async fn binary_reader_scans_lines_like_decoding_the_whole_file() {
    let dir = TempDir::new("env-binary-scan");
    let env = create_env(&dir);
    // A byte-order mark, an invalid sequence before a newline, an empty line,
    // a later U+FEFF, and no final newline.
    let bytes: Vec<u8> = vec![
        0xef, 0xbb, 0xbf, 0x61, 0x0a, 0xe2, 0x82, 0x0a, 0x0a, 0xef, 0xbb, 0xbf, 0x62, 0x0a, 0xc3,
        0xa9,
    ];
    write(&env, "lines.txt", FileContent::Bytes(bytes.clone()));
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    let decoded = decoded
        .strip_prefix('\u{feff}')
        .unwrap_or(&decoded)
        .to_string();
    let lines: Vec<&str> = decoded.split('\n').collect();
    let reader = env
        .open_binary_reader("lines.txt", None, &context())
        .await
        .expect("reader");
    for (start, end) in [
        (0u64, None),
        (0, Some(1)),
        (1, Some(3)),
        (2, Some(3)),
        (3, None),
        (4, Some(9)),
    ] {
        let scan = reader
            .scan_lines(
                ScanLinesOptions {
                    start_line: start,
                    end_line: end,
                },
                &context(),
            )
            .await
            .expect("scan");
        let selected = match end {
            Some(end) => lines
                .get(start as usize..(end as usize).min(lines.len()))
                .unwrap_or_default(),
            None => lines.get(start as usize..).unwrap_or_default(),
        };
        assert_eq!(scan.newlines, lines.len() as u64 - 1);
        let range = |from: u64, to: u64| {
            let slice = &bytes[from as usize..to as usize];
            let mut decoder = rpi_durable::env::decode::Utf8StreamDecoder::new();
            let mut text = decoder.decode(slice);
            text.push_str(&decoder.finish());
            if from == 0 && text.starts_with('\u{feff}') {
                text.remove(0);
            }
            text
        };
        assert_eq!(range(scan.start, scan.end), selected.join("\n"));
        assert_eq!(scan.selected_bytes, selected.join("\n").len() as u64);
        if let Some(first) = lines.get(start as usize) {
            assert_eq!(range(scan.start, scan.first_line_end), *first);
            assert_eq!(scan.first_line_bytes, first.len() as u64);
        }
    }
    let past = reader
        .scan_lines(
            ScanLinesOptions {
                start_line: 9,
                end_line: None,
            },
            &context(),
        )
        .await
        .expect("scan");
    assert_eq!(past.start, bytes.len() as u64);
    assert_eq!(past.end, bytes.len() as u64);
    assert_eq!(past.selected_bytes, 0);
    assert_eq!(
        error_code(
            &reader
                .scan_lines(
                    ScanLinesOptions {
                        start_line: 2,
                        end_line: Some(2)
                    },
                    &context()
                )
                .await
        ),
        Some("invalid")
    );
    reader.close(&context()).await;
}

#[tokio::test]
async fn binary_reader_keeps_reading_the_file_it_opened_after_a_rename() {
    let dir = TempDir::new("env-binary-rename");
    let env = create_env(&dir);
    write(&env, "a.txt", "one");
    let reader = env
        .open_binary_reader("a.txt", None, &context())
        .await
        .expect("reader");
    env.rename_file("a.txt", "b.txt", &context())
        .await
        .expect("rename");
    write(&env, "a.txt", "two");
    assert_eq!(reader.read(0, 10, &context()).await.expect("read"), b"one");
    reader.close(&context()).await;
}

#[tokio::test]
async fn binary_reader_refuses_directories_missing_files_and_aborted_opens() {
    let dir = TempDir::new("env-binary-refuse");
    let env = create_env(&dir);
    env.create_dir("dir", None, &context()).await.expect("dir");
    write(&env, "file.txt", "x");
    assert_eq!(
        error_code(&env.open_binary_reader("dir", None, &context()).await),
        Some("is_directory")
    );
    assert_eq!(
        error_code(
            &env.open_binary_reader("missing.txt", None, &context())
                .await
        ),
        Some("not_found")
    );
    assert_eq!(
        error_code(
            &env.open_binary_reader("file.txt", None, &aborted_context())
                .await
        ),
        Some("aborted")
    );
}

async fn read_all(env: &NodeExecutionEnv, path: &str, max_entries: usize) -> (Vec<DirPage>, bool) {
    let reader = env
        .open_dir_reader(path, &context())
        .await
        .expect("dir reader");
    let mut pages = Vec::new();
    for _ in 0..1000 {
        let page = reader.next(max_entries, &context()).await.expect("page");
        let done = page.done;
        pages.push(page);
        if done {
            return (pages, true);
        }
    }
    (pages, false)
}

#[tokio::test]
async fn directory_reader_pages_every_entry_exactly_once() {
    let dir = TempDir::new("env-dir-pages");
    let env = create_env(&dir);
    let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
    for name in names {
        write(&env, name, name);
    }
    env.create_dir("sub", None, &context()).await.expect("sub");
    let (pages, done) = read_all(&env, ".", 2).await;
    assert!(done, "directory reader reached the end");
    for page in &pages {
        assert!(page.entries.len() <= 2, "page within maxEntries");
    }
    let mut entries: Vec<String> = pages
        .into_iter()
        .flat_map(|page| page.entries)
        .map(|entry| entry.name)
        .collect();
    entries.sort();
    let mut expected = names
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    expected.push("sub".to_string());
    expected.sort();
    assert_eq!(entries, expected);
}

#[tokio::test]
async fn directory_reader_reports_the_end_and_refuses_use_after_close() {
    let dir = TempDir::new("env-dir-end");
    let env = create_env(&dir);
    env.create_dir("empty", None, &context())
        .await
        .expect("empty");
    let reader = env
        .open_dir_reader("empty", &context())
        .await
        .expect("reader");
    let page = reader.next(10, &context()).await.expect("page");
    assert!(page.entries.is_empty());
    assert!(page.done);
    let page = reader.next(10, &context()).await.expect("page");
    assert!(page.entries.is_empty());
    assert!(page.done);
    assert_eq!(
        error_code(&reader.next(0, &context()).await),
        Some("invalid")
    );
    assert_eq!(
        error_code(&reader.next(1, &aborted_context()).await),
        Some("aborted")
    );
    reader.close(&context()).await;
    reader.close(&context()).await;
    assert_eq!(
        error_code(&reader.next(1, &context()).await),
        Some("invalid")
    );
}

#[tokio::test]
async fn directory_reader_refuses_missing_paths_and_files() {
    let dir = TempDir::new("env-dir-refuse");
    let env = create_env(&dir);
    write(&env, "file.txt", "x");
    assert_eq!(
        error_code(&env.open_dir_reader("missing", &context()).await),
        Some("not_found")
    );
    assert_eq!(
        error_code(&env.open_dir_reader("file.txt", &context()).await),
        Some("not_directory")
    );
    assert_eq!(
        error_code(&env.open_dir_reader(".", &aborted_context()).await),
        Some("aborted")
    );
}

#[tokio::test]
async fn directory_reader_skips_entries_removed_during_enumeration() {
    let dir = TempDir::new("env-dir-removed");
    let env = create_env(&dir);
    env.create_dir("dir", None, &context()).await.expect("dir");
    for name in ["x", "y", "z"] {
        write(&env, &format!("dir/{name}"), name);
    }
    let reader = env
        .open_dir_reader("dir", &context())
        .await
        .expect("reader");
    for name in ["x", "y", "z"] {
        env.remove(&format!("dir/{name}"), None, &context())
            .await
            .expect("remove");
    }
    let mut entries = Vec::new();
    for _ in 0..10 {
        let page = reader.next(10, &context()).await.expect("page");
        entries.extend(page.entries);
        if page.done {
            break;
        }
    }
    assert!(entries.is_empty());
    reader.close(&context()).await;
}

// ---------------------------------------------------------------------------
// Watch
// ---------------------------------------------------------------------------

fn covers(change: &WatchChange, path: &str) -> bool {
    match change {
        WatchChange::Overflow => true,
        WatchChange::Error(_) => false,
        WatchChange::Paths(paths) => paths.iter().any(|reported| {
            path == reported
                || path.starts_with(&format!("{reported}/"))
                || path.starts_with(&format!("{reported}\\"))
        }),
    }
}

struct WatchHelper {
    changes: Arc<Mutex<Vec<WatchChange>>>,
    cwd: String,
}

impl WatchHelper {
    fn absolute(&self, path: &str) -> String {
        Path::new(&self.cwd)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }

    /// Wait up to five seconds for a change, made by `change`, that reports
    /// `path`.
    async fn expect_change<F, Fut>(&self, path: &str, change: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let target = self.absolute(path);
        let from = self.changes.lock().expect("changes").len();
        change().await;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let covered = {
                let changes = self.changes.lock().expect("changes");
                changes[from..].iter().any(|entry| covers(entry, &target))
            };
            if covered {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no change reported {target}; got {:#?}",
                self.changes.lock().expect("changes")[from..].to_vec()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn assert_no_excluded(&self, excluded: &[String]) {
        let changes = self.changes.lock().expect("changes");
        for change in changes.iter() {
            if let WatchChange::Paths(paths) = change {
                for path in paths {
                    for prefix in excluded {
                        assert!(
                            path != prefix && !path.starts_with(&format!("{prefix}/")),
                            "excluded {path}"
                        );
                    }
                }
            }
        }
    }
}

async fn watching<F, Fut>(env: Arc<NodeExecutionEnv>, targets: Vec<WatchTarget>, run: F)
where
    F: FnOnce(Arc<NodeExecutionEnv>, WatchHelper) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let changes = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&changes);
    let callback = Arc::new(move |change: WatchChange| {
        captured.lock().expect("changes").push(change);
    });
    let watcher = env
        .watch(targets, callback, &context())
        .await
        .expect("watch");
    let helper = WatchHelper {
        changes,
        cwd: env.cwd(),
    };
    run(Arc::clone(&env), helper).await;
    watcher.close(&context()).await;
}

fn watch_target(path: &str) -> WatchTarget {
    WatchTarget {
        path: path.to_string(),
        recursive: false,
        exclude: Default::default(),
    }
}

#[tokio::test]
async fn watch_reports_a_missing_files_creation_changes_replacement_and_removal() {
    let dir = TempDir::new("env-watch-file");
    let env = create_env(&dir);
    watching(
        Arc::clone(&env),
        vec![watch_target("AGENTS.md")],
        |env, watch| async move {
            watch
                .expect_change("AGENTS.md", || async {
                    write(&env, "AGENTS.md", "one");
                })
                .await;
            watch
                .expect_change("AGENTS.md", || async {
                    write(&env, "AGENTS.md", "two!");
                })
                .await;
            // Editors replace a file by renaming a new one over it.
            watch
                .expect_change("AGENTS.md", || async {
                    write(&env, "AGENTS.md.tmp", "three");
                    env.rename_file("AGENTS.md.tmp", "AGENTS.md", &context())
                        .await
                        .expect("rename");
                })
                .await;
            watch
                .expect_change("AGENTS.md", || async {
                    write(&env, "AGENTS.md", "four");
                })
                .await;
            watch
                .expect_change("AGENTS.md", || async {
                    env.remove("AGENTS.md", None, &context())
                        .await
                        .expect("remove");
                })
                .await;
        },
    )
    .await;
}

#[tokio::test]
async fn watch_reports_a_missing_target_whose_ancestors_are_created() {
    let dir = TempDir::new("env-watch-ancestors");
    let env = create_env(&dir);
    watching(
        Arc::clone(&env),
        vec![watch_target("a/b/c/AGENTS.md")],
        |env, watch| async move {
            watch
                .expect_change("a/b/c/AGENTS.md", || async {
                    write(&env, "a/b/c/AGENTS.md", "x");
                })
                .await;
        },
    )
    .await;
}

#[tokio::test]
async fn watch_follows_directories_created_together_with_their_contents() {
    let dir = TempDir::new("env-watch-newdir");
    let env = create_env(&dir);
    env.create_dir("skills", None, &context())
        .await
        .expect("skills");
    let target = WatchTarget {
        path: "skills".to_string(),
        recursive: true,
        exclude: Default::default(),
    };
    watching(Arc::clone(&env), vec![target], |env, watch| async move {
        // Written before any watcher on the new directories can exist.
        watch
            .expect_change("skills/a/b/SKILL.md", || async {
                write(&env, "skills/a/b/SKILL.md", "one");
            })
            .await;
        watch
            .expect_change("skills/a/b/SKILL.md", || async {
                write(&env, "skills/a/b/SKILL.md", "two!");
            })
            .await;
        watch
            .expect_change("skills/a/b/c/SKILL.md", || async {
                write(&env, "skills/a/b/c/SKILL.md", "deeper");
            })
            .await;
    })
    .await;
}

#[tokio::test]
async fn watch_keeps_watching_a_path_whose_parent_is_renamed_and_recreated() {
    let dir = TempDir::new("env-watch-parent");
    let env = create_env(&dir);
    write(&env, "proj/.pi/skills/x.md", "x");
    let target = WatchTarget {
        path: "proj/.pi/skills".to_string(),
        recursive: true,
        exclude: Default::default(),
    };
    watching(Arc::clone(&env), vec![target], |env, watch| async move {
        watch
            .expect_change("proj/.pi/skills", || async {
                env.rename_file("proj/.pi", "proj/old", &context())
                    .await
                    .expect("rename");
            })
            .await;
        watch
            .expect_change("proj/.pi/skills/y.md", || async {
                write(&env, "proj/.pi/skills/y.md", "y");
            })
            .await;
        watch
            .expect_change("proj/.pi/skills/y.md", || async {
                write(&env, "proj/.pi/skills/y.md", "yy");
            })
            .await;
    })
    .await;
}

#[tokio::test]
async fn watch_skips_excluded_entries_and_reports_a_rename_out_of_them() {
    let dir = TempDir::new("env-watch-exclude");
    let env = create_env(&dir);
    env.create_dir("skills", None, &context())
        .await
        .expect("skills");
    let target = WatchTarget {
        path: "skills".to_string(),
        recursive: true,
        exclude: rpi_durable::env::types::ExcludeOptions {
            hidden: true,
            names: vec!["node_modules".to_string()],
        },
    };
    watching(Arc::clone(&env), vec![target], |env, watch| async move {
        write(&env, "skills/node_modules/dep/SKILL.md", "dep");
        write(&env, "skills/.SKILL.md.tmp", "draft");
        watch
            .expect_change("skills/SKILL.md", || async {
                env.rename_file("skills/.SKILL.md.tmp", "skills/SKILL.md", &context())
                    .await
                    .expect("rename");
            })
            .await;
        watch.assert_no_excluded(&[
            watch.absolute("skills/node_modules"),
            watch.absolute("skills/.SKILL.md.tmp"),
        ]);
    })
    .await;
}

#[tokio::test]
async fn watch_keeps_recursive_coverage_where_a_non_recursive_target_overlaps() {
    let dir = TempDir::new("env-watch-overlap");
    let env = create_env(&dir);
    write(&env, "skills/a/one.md", "one");
    let targets = vec![
        watch_target("skills"),
        WatchTarget {
            path: "skills".to_string(),
            recursive: true,
            exclude: Default::default(),
        },
    ];
    watching(Arc::clone(&env), targets, |env, watch| async move {
        watch
            .expect_change("skills/a/two.md", || async {
                write(&env, "skills/a/two.md", "two");
            })
            .await;
    })
    .await;
}

#[tokio::test]
async fn watch_follows_a_directory_replaced_at_the_same_path() {
    let dir = TempDir::new("env-watch-replaced");
    let env = create_env(&dir);
    write(&env, "skills/a/x.md", "x");
    let target = WatchTarget {
        path: "skills".to_string(),
        recursive: true,
        exclude: Default::default(),
    };
    watching(Arc::clone(&env), vec![target], |env, watch| async move {
        watch
            .expect_change("skills/a", || async {
                env.rename_file("skills/a", "skills-old", &context())
                    .await
                    .expect("rename");
                env.create_dir("skills/a", None, &context())
                    .await
                    .expect("mkdir");
            })
            .await;
        watch
            .expect_change("skills/a/y.md", || async {
                write(&env, "skills/a/y.md", "y");
            })
            .await;
        watch
            .expect_change("skills/a/y.md", || async {
                write(&env, "skills/a/y.md", "yy");
            })
            .await;
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn watch_reports_an_error_and_stops_when_the_target_becomes_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("env-watch-error");
    let env = create_env(&dir);
    env.create_dir("watched", None, &context())
        .await
        .expect("watched");
    write(&env, "watched/file.txt", "x");
    watching(
        Arc::clone(&env),
        vec![watch_target("watched")],
        |env, watch| async move {
            std::fs::set_permissions(
                dir.path().join("watched"),
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("chmod");
            // Root and permissive file systems ignore the mode: nothing to assert.
            if std::fs::read_dir(dir.path().join("watched")).is_ok() {
                std::fs::set_permissions(
                    dir.path().join("watched"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .expect("restore");
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let saw_error = {
                    let changes = watch.changes.lock().expect("changes");
                    changes
                        .iter()
                        .any(|change| matches!(change, WatchChange::Error(_)))
                };
                if saw_error {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "no error change reported: {:#?}",
                    watch.changes.lock().expect("changes").to_vec()
                );
                // Prod the ancestor watcher so a rescan happens.
                write(&env, "probe.txt", "p");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            std::fs::set_permissions(
                dir.path().join("watched"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("restore");
        },
    )
    .await;
}

#[tokio::test]
async fn watch_stops_reporting_once_closed() {
    let dir = TempDir::new("env-watch-close");
    let env = create_env(&dir);
    let changes = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&changes);
    let callback = Arc::new(move |change: WatchChange| {
        captured.lock().expect("changes").push(change);
    });
    let watcher = env
        .watch(vec![watch_target("file.txt")], callback, &context())
        .await
        .expect("watch");
    assert!(matches!(
        watcher.mode(),
        rpi_durable::env::types::WatchMode::Native | rpi_durable::env::types::WatchMode::Polling
    ));
    watcher.close(&context()).await;
    watcher.close(&context()).await;
    write(&env, "file.txt", "x");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(changes.lock().expect("changes").is_empty());
}

// ---------------------------------------------------------------------------
// Exec
// ---------------------------------------------------------------------------

struct Collected {
    result: Result<ShellExecResult, rpi_durable::env::types::ExecutionError>,
    stdout: String,
    stderr: String,
}

async fn exec_collect(
    env: &NodeExecutionEnv,
    command: ShellCommand,
    cwd: Option<&str>,
) -> Collected {
    let output: Arc<Mutex<(String, String)>> = Arc::new(Mutex::new((String::new(), String::new())));
    let captured = Arc::clone(&output);
    let options = ShellExecOptions {
        cwd: cwd.map(str::to_string),
        on_output: Some(Arc::new(
            move |text: &str, _context: &Context, info: ShellOutputInfo| {
                let mut guard = captured.lock().expect("output");
                match info.stream {
                    ShellStream::Stdout => guard.0.push_str(text),
                    ShellStream::Stderr => guard.1.push_str(text),
                }
            },
        )),
        ..Default::default()
    };
    let result = env.exec(command, Some(options), &context()).await;
    let guard = output.lock().expect("output");
    Collected {
        result,
        stdout: guard.0.clone(),
        stderr: guard.1.clone(),
    }
}

fn argv(parts: &[&str]) -> ShellCommand {
    ShellCommand::Argv(parts.iter().map(|part| part.to_string()).collect())
}

#[tokio::test]
async fn argv_exec_passes_arguments_without_shell_parsing() {
    let dir = TempDir::new("env-exec-argv");
    let env = create_env(&dir);
    let hostile = "it's $(touch pwned) `touch pwned` *; touch pwned";
    let collected = exec_collect(
        &env,
        argv(&[
            "sh",
            "-c",
            "printf \"%s|%s\" \"$1\" \"$2\"",
            "argv0",
            hostile,
            "a b",
        ]),
        None,
    )
    .await;
    assert_eq!(collected.result.expect("exec").exit_code, 0);
    assert_eq!(collected.stdout, format!("{hostile}|a b"));
    assert!(!env.exists("pwned", &context()).await.expect("exists"));
}

#[tokio::test]
async fn exec_reports_the_stream_of_every_chunk_in_both_forms() {
    let dir = TempDir::new("env-exec-streams");
    let env = create_env(&dir);
    let script = "printf out; printf err >&2; printf more";
    let as_argv = exec_collect(&env, argv(&["sh", "-c", script]), None).await;
    assert_eq!(as_argv.result.expect("argv").exit_code, 0);
    assert_eq!(as_argv.stdout, "outmore");
    assert_eq!(as_argv.stderr, "err");
    let as_string = exec_collect(&env, ShellCommand::Shell(script.to_string()), None).await;
    assert_eq!(as_string.result.expect("string").exit_code, 0);
    assert_eq!(as_string.stdout, "outmore");
    assert_eq!(as_string.stderr, "err");
}

#[tokio::test]
async fn argv_exec_honors_cwd_and_exit_codes() {
    let dir = TempDir::new("env-exec-cwd");
    let env = create_env(&dir);
    env.create_dir("sub", None, &context()).await.expect("sub");
    let made = exec_collect(
        &env,
        argv(&["sh", "-c", "printf x > made.txt; exit 3"]),
        Some("sub"),
    )
    .await;
    assert_eq!(made.result.expect("exec").exit_code, 3);
    assert_eq!(
        env.read_text_file("sub/made.txt", &context())
            .await
            .expect("read"),
        "x"
    );
}

#[tokio::test]
async fn argv_exec_reports_missing_programs_and_empty_argv_as_spawn_errors() {
    let dir = TempDir::new("env-exec-spawn");
    let env = create_env(&dir);
    let missing = env
        .exec(
            argv(&["pi-durable-conformance-missing-program"]),
            None,
            &context(),
        )
        .await
        .expect_err("spawn error");
    assert_eq!(missing.code, ExecutionErrorCode::SpawnError);
    let empty = env
        .exec(ShellCommand::Argv(Vec::new()), None, &context())
        .await
        .expect_err("spawn error");
    assert_eq!(empty.code, ExecutionErrorCode::SpawnError);
}

#[tokio::test]
async fn windowed_exec_keeps_the_exact_tail_and_counts_what_it_skips() {
    let dir = TempDir::new("env-exec-window");
    let env = create_env(&dir);
    let lines: u64 = 2000;
    let window = ShellOutputWindow {
        max_bytes: 200,
        max_lines: 5,
        min_interval_ms: 0,
        bytes_per_second: 1_000_000_000,
    };
    let state: Arc<Mutex<(u64, u64, String)>> = Arc::new(Mutex::new((0, 0, String::new())));
    let captured = Arc::clone(&state);
    let options = ShellExecOptions {
        window: Some(window),
        on_output: Some(Arc::new(
            move |text: &str, _context: &Context, info: ShellOutputInfo| {
                let mut guard = captured.lock().expect("state");
                if let Some(skipped) = info.skipped {
                    guard.0 += skipped.bytes;
                    guard.1 += skipped.newlines;
                    guard.2.clear();
                }
                guard.0 += text.len() as u64;
                guard.1 += text.matches('\n').count() as u64;
                guard.2.push_str(text);
            },
        )),
        ..Default::default()
    };
    let script = format!("i=0; while [ $i -lt {lines} ]; do echo line-$i; i=$((i+1)); done");
    let result = env
        .exec(ShellCommand::Shell(script), Some(options), &context())
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);
    let guard = state.lock().expect("state");
    let expected: String = (0..lines).map(|index| format!("line-{index}\n")).collect();
    assert_eq!(guard.0, expected.len() as u64);
    assert_eq!(guard.1, lines);
    let tail: String = (lines - window.max_lines..lines)
        .map(|index| format!("line-{index}\n"))
        .collect();
    assert!(
        guard.2.ends_with(&tail),
        "delivered output ends with the tail"
    );
}

#[tokio::test]
async fn argv_exec_distinguishes_timeout_from_abort() {
    let dir = TempDir::new("env-exec-timeout");
    let env = create_env(&dir);
    let timed_out = env
        .exec(
            argv(&["sh", "-c", "sleep 2"]),
            Some(ShellExecOptions {
                timeout: Some(0.1),
                ..Default::default()
            }),
            &context(),
        )
        .await
        .expect_err("timeout");
    assert_eq!(timed_out.code, ExecutionErrorCode::Timeout);

    let token = CancellationToken::new();
    let aborted = with_abort_signal(token.clone(), &BACKGROUND_CONTEXT);
    let running = env.exec(argv(&["sh", "-c", "sleep 2"]), None, &aborted);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });
    let error = running.await.expect_err("abort");
    assert_eq!(error.code, ExecutionErrorCode::Aborted);
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn binary_reader_follows_symlinks_unless_nofollow_refuses_the_final_one() {
    let dir = TempDir::new("env-link-reader");
    let env = create_env(&dir);
    write(&env, "target.txt", "target");
    env.create_dir("sub", None, &context()).await.expect("sub");
    write(&env, "sub/inner.txt", "inner");
    std::os::unix::fs::symlink("target.txt", dir.path().join("link.txt")).expect("link");
    std::os::unix::fs::symlink("sub", dir.path().join("dirlink")).expect("dirlink");

    let followed = env
        .open_binary_reader("link.txt", None, &context())
        .await
        .expect("follow");
    assert_eq!(
        followed.read(0, 10, &context()).await.expect("read"),
        b"target"
    );
    followed.close(&context()).await;

    assert_eq!(
        error_code(
            &env.open_binary_reader(
                "link.txt",
                Some(rpi_durable::env::types::OpenBinaryReaderOptions { no_follow: true }),
                &context()
            )
            .await
        ),
        Some("invalid")
    );

    // Only the final component is refused; earlier symlinked directories
    // still resolve.
    let inner = env
        .open_binary_reader(
            "dirlink/inner.txt",
            Some(rpi_durable::env::types::OpenBinaryReaderOptions { no_follow: true }),
            &context(),
        )
        .await
        .expect("inner");
    assert_eq!(inner.read(0, 10, &context()).await.expect("read"), b"inner");
    inner.close(&context()).await;
}

#[cfg(unix)]
#[tokio::test]
async fn watch_reports_changes_to_the_file_a_watched_symbolic_link_points_to() {
    let dir = TempDir::new("env-link-watch");
    let env = create_env(&dir);
    write(&env, "data/real.md", "one");
    env.create_dir("config", None, &context())
        .await
        .expect("config");
    std::os::unix::fs::symlink("../data/real.md", dir.path().join("config/AGENTS.md"))
        .expect("link");
    watching(
        Arc::clone(&env),
        vec![watch_target("config/AGENTS.md")],
        |env, watch| async move {
            watch
                .expect_change("config/AGENTS.md", || async {
                    write(&env, "data/real.md", "two!");
                })
                .await;
        },
    )
    .await;
}

/// Keep the helper imports used across cfg gates.
#[allow(dead_code)]
fn _unused(_: CreateDirOptions, _: RemoveOptions) {}
