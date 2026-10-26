//! `NodeExecutionEnv::create_temp_dir` / `create_temp_file` hardening:
//! private modes, collision-resistant names, and `create_new` semantics.

use std::path::Path;

use rpi_durable::chord::context::BACKGROUND_CONTEXT;
use rpi_durable::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
use rpi_durable::env::types::{CreateTempFileOptions, FileSystem};

fn env() -> NodeExecutionEnv {
    NodeExecutionEnv::new(NodeExecutionEnvOptions::default())
}

#[cfg(unix)]
fn mode(path: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test]
async fn temp_dirs_are_private_and_unique() {
    let env = env();
    let mut paths = Vec::new();
    for _ in 0..16 {
        let path = env
            .create_temp_dir(Some("rpi-durable-probe-"), &BACKGROUND_CONTEXT)
            .await
            .expect("temp dir");
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o700, "temp dir must be 0700: {path}");
        paths.push(path);
    }
    let unique: std::collections::HashSet<_> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "temp dir names collided");
    for path in paths {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn temp_files_are_private_and_unique() {
    let env = env();
    let options = CreateTempFileOptions {
        prefix: Some("probe-".to_owned()),
        suffix: Some(".log".to_owned()),
    };
    let mut paths = Vec::new();
    for _ in 0..16 {
        let path = env
            .create_temp_file(Some(options.clone()), &BACKGROUND_CONTEXT)
            .await
            .expect("temp file");
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600, "temp file must be 0600: {path}");
        assert!(
            Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("probe-") && name.ends_with(".log")),
            "prefix/suffix preserved: {path}"
        );
        paths.push(path);
    }
    let unique: std::collections::HashSet<_> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "temp file names collided");
    for path in paths {
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
