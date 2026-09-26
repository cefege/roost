//! The git facts about a session's folder, read on the worker host because a
//! browser cannot shell out. A branch name and a GitHub remote feed the folder
//! row's subtitle. Depends on `host::tool_path` for the bounded runner and on
//! nothing here.
//!
//! EVERY FAILURE IS `None`. A missing `git`, a folder that is not a repository,
//! a repository with no `origin` — all of them are "nothing to show", and none
//! of them is worth an error on a poll that runs every ninety seconds.
//!
//! THE PROGRAM IS A FIELD. `git` is resolved by the reader that is given it, so
//! a test drives a script instead of whatever happens to be on this machine's
//! `PATH`, and a worker can be pointed at a specific git without a second
//! resolution rule.

use std::path::{Path, PathBuf};

use super::tool_path;

/// One `git` invocation, and the answer this module draws from it.
#[derive(Debug, Clone)]
pub struct GitReader {
    program: PathBuf,
}

impl GitReader {
    /// A reader that runs this program as `git`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// A reader that resolves `git` against the worker's own tool `PATH`.
    #[must_use]
    pub fn from_tool_path(inherited: Option<&str>, platform: roost_host::HostPlatform) -> Self {
        Self::new(tool_path::tool_path(inherited, platform))
    }

    /// The current branch of `cwd`, or `None` when it is not a repository.
    ///
    /// A detached HEAD reads as `@<short-sha>` rather than as nothing: the row
    /// then shows something stable and true instead of an empty subtitle, and
    /// a checkout to a commit is visibly not a branch.
    #[must_use]
    pub fn branch(&self, cwd: &str) -> Option<String> {
        let reference = self.run(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if reference != "HEAD" {
            return Some(reference);
        }
        self.run(cwd, &["rev-parse", "--short", "HEAD"])
            .map(|sha| format!("@{sha}"))
    }

    /// The `owner/repo` of the `origin` remote, or `None`.
    ///
    /// Both spellings are read: `git@github.com:owner/repo.git` and
    /// `https://github.com/owner/repo.git`. A non-GitHub remote is `None`
    /// rather than a guess, because the value is only ever used to ask GitHub
    /// about a pull request.
    #[must_use]
    pub fn remote(&self, cwd: &str) -> Option<String> {
        let url = self.run(cwd, &["remote", "get-url", "origin"])?;
        github_owner_repo(&url)
    }

    /// The `HEAD` file of the repository at `cwd`, or `None`.
    ///
    /// Read through `rev-parse --git-path HEAD` rather than by joining `.git`:
    /// in a linked worktree `.git` is a FILE, so the naive path does not exist
    /// and a watcher on it silently watches nothing.
    #[must_use]
    pub fn head_path(&self, cwd: &str) -> Option<PathBuf> {
        let relative = self.run(cwd, &["rev-parse", "--git-path", "HEAD"])?;
        let path = Path::new(cwd).join(relative);
        Some(path)
    }

    /// One `git` answer, trimmed, or `None` for a non-zero exit or empty output.
    ///
    /// Run in `cwd` rather than pointed at it: `git rev-parse` answers about
    /// the repository it is standing in, and `-C` is not accepted by every git
    /// a worker's service account may have.
    fn run(&self, cwd: &str, args: &[&str]) -> Option<String> {
        tool_path::run(
            &self.program.display().to_string(),
            args,
            Some(Path::new(cwd)),
        )
        .map(|out| out.trim().to_string())
        .filter(|out| !out.is_empty())
    }
}

/// The `owner/repo` of a GitHub remote URL, or `None`.
#[must_use]
pub fn github_owner_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/');
    let owner = parts.next()?.trim();
    let repository = parts.next()?.trim();
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    if owner.is_empty() || repository.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}
