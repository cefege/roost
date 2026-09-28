//! The pull-request badge on a folder row, read through the `gh` CLI on the
//! worker host. A browser cannot shell out, and only this worker can reach both
//! `gh` and the repository. Depends on `roost_protocol::wire::session` for the
//! two enums this projects into, on `serde_json` for what `gh` prints, and on
//! `host::tool_path` for the runner — and on nothing here.
//!
//! EVERY FAILURE IS `None`, NEVER AN ERROR. `gh` missing, not authenticated, no
//! pull request for the branch, a network that is down, output that will not
//! parse: all of them resolve to no badge. A folder row that is briefly
//! unbadged is a far smaller incident than a spawn that throws because GitHub
//! is unreachable, and a poll that could fail is a poll every call site has to
//! defend instead of one.
//!
//! `gh` IS NOT ON A WORKER'S `PATH`. It lives in `/opt/homebrew/bin` on Apple
//! silicon and `/usr/local/bin` on Intel, and a launchd or systemd unit
//! carries neither, so a bare spawn ENOENTs and the badge silently never
//! resolves. The production reader runs `gh` with the tool `PATH` as ITS
//! `PATH` (v2 `GH_PATH`, `apps/worker/src/host/pr-status.ts`); a test drives a
//! script by path rather than whatever this machine has installed.

use std::path::{Path, PathBuf};

use roost_protocol::wire::session::{PullRequestChecks, PullRequestState};
use serde_json::Value;

use super::PrStatus;
use super::tool_path::{self, TOOL_TIMEOUT};

/// One entry of `gh`'s `statusCheckRollup`, which mixes two shapes.
///
/// A check RUN carries `status` and `conclusion`; a legacy commit STATUS
/// carries `state`. Both appear in the same array, which is why this is one
/// struct with optional fields rather than an enum. `conclusion` keeps absent
/// apart from empty because v2 reads `conclusion ?? state`: an absent (or
/// null) conclusion falls back to the state, an empty one does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollupEntry {
    /// `QUEUED`, `IN_PROGRESS`, `COMPLETED`; empty when absent.
    pub status: String,
    /// `SUCCESS`, `FAILURE`, `CANCELLED`, … once a run is `COMPLETED`.
    pub conclusion: Option<String>,
    /// `SUCCESS`, `PENDING`, `ERROR`. Absent on a check run.
    pub state: Option<String>,
}

/// One `gh` invocation and the status it reports.
#[derive(Debug, Clone)]
pub struct PrReader {
    program: PathBuf,
    /// The `PATH` the program runs with and is looked up on; `None` inherits
    /// this process's.
    path: Option<String>,
}

impl PrReader {
    /// A reader that runs this program as `gh`, on this process's `PATH`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            path: None,
        }
    }

    /// The production reader: `gh` run with `path` as its `PATH`, over
    /// [`tool_path::process_tool_path`].
    #[must_use]
    pub fn on_path(path: String) -> Self {
        Self {
            program: PathBuf::from("gh"),
            path: Some(path),
        }
    }

    /// The status of the pull request for `branch` in the repository at `cwd`,
    /// or `None` for every reason there is not one.
    #[must_use]
    pub fn status(&self, cwd: &str, branch: &str) -> Option<PrStatus> {
        let out = tool_path::run_bounded(
            &self.program.display().to_string(),
            &[
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "all",
                "--limit",
                "1",
                "--json",
                "number,state,isDraft,url,statusCheckRollup",
            ],
            Some(Path::new(cwd)),
            self.path.as_deref(),
            TOOL_TIMEOUT,
        )?;
        parse_pull_request(&out)
    }
}

/// The first row of a `gh pr list` answer, or `None` for anything else.
///
/// Empty output and an empty array are the same answer — no pull request — and
/// both are what a branch with no PR produces, so they are not distinguished.
#[must_use]
pub fn parse_pull_request(out: &str) -> Option<PrStatus> {
    let value: Value = serde_json::from_str(out.trim()).ok()?;
    let row = value.as_array()?.first()?;
    Some(PrStatus {
        number: u32::try_from(row.get("number")?.as_u64()?).ok()?,
        state: pull_request_state(
            row.get("state")?.as_str()?,
            row.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        ),
        checks: rollup_checks(&rollup_entries(row)),
        url: row.get("url")?.as_str()?.to_string(),
    })
}

/// The rollup of a row, or an empty one when it carries none.
fn rollup_entries(row: &Value) -> Vec<RollupEntry> {
    let Some(entries) = row.get("statusCheckRollup").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| RollupEntry {
            status: entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            conclusion: entry
                .get("conclusion")
                .and_then(Value::as_str)
                .map(str::to_string),
            state: entry
                .get("state")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect()
}

/// `gh`'s `state` and `isDraft` as the protocol's own enum.
///
/// A state this build has never heard of reads as `open`: a pull request `gh`
/// reported and this worker cannot classify is still an open pull request, and
/// the alternative is losing the badge over a spelling.
#[must_use]
pub fn pull_request_state(raw: &str, is_draft: bool) -> PullRequestState {
    match raw.trim().to_ascii_uppercase().as_str() {
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => PullRequestState::Closed,
        _ if is_draft => PullRequestState::Draft,
        _ => PullRequestState::Open,
    }
}

/// The rollup as the protocol's own enum (v2 `rollupChecks`).
///
/// A failure anywhere is `failing` and outranks a pending sibling: a badge that
/// says "passing" while one required check failed is a lie somebody merges on.
/// A rollup with nothing in it is `none`, not `passing` — no checks is not a
/// passing run. An entry's outcome is its conclusion, or its state when it
/// carries no conclusion at all.
#[must_use]
pub fn rollup_checks(rollup: &[RollupEntry]) -> PullRequestChecks {
    if rollup.is_empty() {
        return PullRequestChecks::None;
    }
    let mut pending = false;
    for entry in rollup {
        let outcome = upper(
            entry
                .conclusion
                .as_deref()
                .or(entry.state.as_deref())
                .unwrap_or_default(),
        );
        let status = upper(&entry.status);
        let state = upper(entry.state.as_deref().unwrap_or_default());
        if matches!(
            outcome.as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED"
        ) {
            return PullRequestChecks::Failing;
        }
        if matches!(status.as_str(), "QUEUED" | "IN_PROGRESS")
            || outcome == "PENDING"
            || state == "PENDING"
        {
            pending = true;
        }
        // No outcome and not completed: a run that has not reported yet.
        if outcome.is_empty() && status != "COMPLETED" && entry.state.is_none() {
            pending = true;
        }
    }
    if pending {
        PullRequestChecks::Pending
    } else {
        PullRequestChecks::Passing
    }
}

fn upper(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}
