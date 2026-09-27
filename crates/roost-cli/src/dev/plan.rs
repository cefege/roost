//! The three dev servers as resolved argv: a coordinator, the worker that
//! dials it, and the web dev server. Called by `dev::run` once the boot has
//! resolved, and by the tests that assert what a child is started with.
//! Depends on nothing but what the boot resolver decided, so no port and no
//! tool name in here is a second copy of a decision somebody else owns.

use std::path::{Path, PathBuf};

/// The coordinator child.
pub const COORDINATOR: &str = "coordinator";
/// The worker child.
pub const WORKER: &str = "worker";
/// The web dev server child.
pub const WEB: &str = "web";

/// The Dioxus CLI. v3's web app is a Rust workspace member, so the dev server is
/// the Dioxus CLI and not the vite the v2 CLI started.
pub const WEB_PROGRAM: &str = "dx";

/// The workspace member the Dioxus CLI is pointed at, and the same package the
/// `dx build` line in `status/render.rs` names: a web dev server that served a
/// different package than the release build would be a second front end.
pub const WEB_PACKAGE: &str = "roost-web";

/// One dev server, as the process that will be started. The name is what an
/// operator sees in a failure line and in the log, so it is a fixed word
/// rather than something derived from the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevServer {
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl DevServer {
    pub fn new(name: &'static str, program: &str, args: &[&str]) -> Self {
        Self {
            name,
            program: PathBuf::from(program),
            args: args
                .iter()
                .map(|argument| (*argument).to_string())
                .collect(),
        }
    }

    /// The command as a person would type it, for a log line and for the
    /// failure message of a program that cannot be run. Nothing reads a
    /// command back out of it.
    pub fn command_line(&self) -> String {
        let mut line = self.program.display().to_string();
        for argument in &self.args {
            line.push(' ');
            line.push_str(argument);
        }
        line
    }
}

/// The URL a worker dials for a coordinator on `bind`. Plain HTTP, because a
/// loopback dev coordinator speaks nothing else; the scheme rewrite to
/// `ws`/`wss` belongs to the worker's own endpoint resolution.
pub fn coordinator_url(bind: &str) -> String {
    format!("http://{bind}")
}

/// The dev stack, in start order.
///
/// The coordinator child carries NO `--bind`: it resolves the same bind this
/// process resolved, out of the same environment, and a flag here would let the
/// two disagree the moment an operator declared `ROOST_COORDINATOR_BIND`. The
/// worker child is told the URL instead, because that is the one fact a child
/// cannot work out for itself — and it is the resolved one, not a port spelled
/// a second time here.
///
/// Every child inherits this process's working directory, so `roost dev` is
/// run from the checkout root the way `cargo run` is: the Dioxus CLI finds the
/// workspace by walking up from where it was started.
pub fn dev_plan(executable: &Path, coordinator_url: &str) -> Vec<DevServer> {
    vec![
        DevServer {
            name: COORDINATOR,
            program: executable.to_path_buf(),
            args: vec!["coord".to_string()],
        },
        DevServer {
            name: WORKER,
            program: executable.to_path_buf(),
            args: vec![
                "worker".to_string(),
                "--coordinator-url".to_string(),
                coordinator_url.to_string(),
            ],
        },
        DevServer {
            name: WEB,
            program: PathBuf::from(WEB_PROGRAM),
            args: vec![
                "serve".to_string(),
                "-p".to_string(),
                WEB_PACKAGE.to_string(),
                "--platform".to_string(),
                "web".to_string(),
            ],
        },
    ]
}
