//! Project instructions and watchdog context collection for a conversation.
//! Called by `ToolHost`; context is for the model and watchdog rules for the advisor.
//! Walks only from the project directory toward its repository or home boundary.

use std::path::Path;

use roost_protocol::wire::agent_chat::{ContextFiles, TOOL_CONTEXT_FILES};
use serde_json::json;

use crate::outcome::ToolOutcome;

const CONTEXT_LIMIT: usize = 64 * 1024;
const WATCHDOG_LIMIT: usize = 32 * 1024;

pub async fn context_files_tool(cwd: &Path) -> ToolOutcome {
    let cwd = cwd.to_path_buf();
    let files = match tokio::task::spawn_blocking(move || collect_context(&cwd)).await {
        Ok(files) => files,
        Err(error) => return ToolOutcome::failure(format!("context file task failed: {error}")),
    };
    let content = serde_json::to_string(&files)
        .unwrap_or_else(|_| "{\"context\":\"\",\"watchdog\":\"\"}".to_owned());
    ToolOutcome::success(content).with_details(json!({"tool": TOOL_CONTEXT_FILES}))
}

pub fn collect_context(cwd: &Path) -> ContextFiles {
    let home = crate::search_path::home_dir();
    let mut current = cwd.to_path_buf();
    let mut context = String::new();
    let mut watchdog = String::new();
    loop {
        append_files(
            &current,
            &["AGENTS.md", "CLAUDE.md"],
            &mut context,
            CONTEXT_LIMIT,
        );
        append_files(
            &current,
            &["WATCHDOG.md", ".omp/WATCHDOG.md"],
            &mut watchdog,
            WATCHDOG_LIMIT,
        );
        if current.join(".git").exists() || home.as_ref().is_some_and(|home| current == *home) {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    if let Some(home) = home {
        append_file(
            &home.join(".omp/agent/WATCHDOG.md"),
            &mut watchdog,
            WATCHDOG_LIMIT,
        );
    }
    ContextFiles { context, watchdog }
}

fn append_files(directory: &Path, names: &[&str], output: &mut String, limit: usize) {
    for name in names {
        append_file(&directory.join(name), output, limit);
    }
}

fn append_file(path: &Path, output: &mut String, limit: usize) {
    if output.len() >= limit {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    let label = path.display();
    let section = format!("<{}>\n{}\n</{}>\n", label, contents, label);
    let remaining = limit - output.len();
    if section.len() <= remaining {
        output.push_str(&section);
    } else {
        output.push_str(&section[..section.floor_char_boundary(remaining)]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn walks_to_repository_and_home_scopes_watchdogs() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        let nested = root.join("sub");
        std::fs::create_dir_all(&nested).expect("directories");
        std::fs::create_dir(root.join(".git")).expect("git marker");
        std::fs::write(root.join("CLAUDE.md"), "repo rules").expect("rules");
        std::fs::write(root.join("WATCHDOG.md"), "repo watchdog").expect("watchdog");
        let context = collect_context(&nested);
        assert!(context.context.contains("repo rules"));
        assert!(context.watchdog.contains("repo watchdog"));
    }

    #[test]
    fn respects_home_boundary() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let nested = home.join("project");
        std::fs::create_dir_all(&nested).expect("directories");
        std::fs::write(home.join("AGENTS.md"), "home instructions").expect("instructions");
        let context = collect_context(&nested);
        assert!(context.context.contains("home instructions"));
    }
}
