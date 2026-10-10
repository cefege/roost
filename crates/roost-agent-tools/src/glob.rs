//! Gitignore-aware path matching for worker tool calls.
//! Called by `ToolHost` to find project files without traversing ignored trees.
//! Results are bounded and ordered by most recent modification time.

use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use globset::Glob;
use ignore::WalkBuilder;
use roost_protocol::wire::agent_chat::{GlobArgs, TOOL_GLOB};
use serde_json::json;

use crate::outcome::ToolOutcome;

const PATH_LIMIT: usize = 500;

pub async fn glob_tool(cwd: &Path, args: GlobArgs) -> ToolOutcome {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || glob_tool_sync(&cwd, args))
        .await
        .unwrap_or_else(|error| ToolOutcome::failure(format!("glob task failed: {error}")))
}

fn glob_tool_sync(cwd: &Path, args: GlobArgs) -> ToolOutcome {
    let root = args
        .path
        .as_deref()
        .map_or_else(|| cwd.to_path_buf(), |path| cwd.join(path));
    let matcher = match Glob::new(&args.pattern) {
        Ok(glob) => glob.compile_matcher(),
        Err(error) => return ToolOutcome::failure(format!("invalid glob: {error}")),
    };
    let mut paths: Vec<(SystemTime, PathBuf)> = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| matcher.is_match(entry.path().strip_prefix(&root).unwrap_or(entry.path())))
        .map(|entry| {
            let path = entry.into_path();
            let modified = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    paths.sort_by_key(|(modified, _)| Reverse(*modified));
    let truncated = paths.len() > PATH_LIMIT;
    paths.truncate(PATH_LIMIT);
    let mut content = paths
        .iter()
        .map(|(_, path)| path.strip_prefix(cwd).unwrap_or(path).display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if content.is_empty() {
        content.push_str("No matching files found.");
    }
    if truncated {
        content.push_str(&format!("\nResults capped at {PATH_LIMIT} paths."));
    }
    ToolOutcome::success(content).with_details(json!({"tool": TOOL_GLOB, "count": paths.len()}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn excludes_git_ignored_paths() {
        let directory = tempdir().expect("tempdir");
        fs::create_dir(directory.path().join(".git")).expect("git directory");
        fs::create_dir(directory.path().join("ignored")).expect("ignored dir");
        fs::write(directory.path().join(".gitignore"), "ignored/\n").expect("gitignore");
        fs::write(directory.path().join("visible.txt"), "visible").expect("visible");
        fs::write(directory.path().join("ignored/hidden.txt"), "ignored").expect("ignored file");
        let result = glob_tool(
            directory.path(),
            GlobArgs {
                pattern: "**/*.txt".to_owned(),
                path: None,
            },
        )
        .await;
        assert!(result.content.contains("visible.txt"));
        assert!(!result.content.contains("hidden.txt"));
    }
}
