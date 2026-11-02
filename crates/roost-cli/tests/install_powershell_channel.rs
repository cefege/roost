#![cfg(windows)]
//! `install.ps1`: which release a first install takes on Windows. The real
//! script's function definitions are loaded through Windows PowerShell's own
//! AST and called against loopback fixtures standing in for GitHub's listing
//! and compare endpoints; the installation entrypoint itself never runs.
//!
//! GitHub's compare endpoint is the only thing that distinguishes a tag cut
//! from `v3` from a side-branch tag whose name merely starts with `v3.`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The URL shape GitHub answers for `<Prefix>/v3...<Tag>`.
const COMPARE_PREFIX: &str = "/compare/v3...";

#[derive(Clone, Copy)]
enum CompareAnswer {
    Status(&'static str),
    Unreadable,
}

#[derive(Default)]
struct ReleaseOrigin {
    listings: BTreeMap<String, Vec<String>>,
    compares: BTreeMap<String, CompareAnswer>,
}

/// One request from the script: a published-releases listing, a compare
/// answer, or a 404 for anything the test did not publish.
async fn answer(State(origin): State<Arc<ReleaseOrigin>>, request: Request) -> Response {
    let path = request.uri().path();
    if let Some(tags) = origin.listings.get(path) {
        let releases: Vec<_> = tags.iter().map(|tag| json!({ "tag_name": tag })).collect();
        return Json(releases).into_response();
    }
    if let Some(tag) = path.strip_prefix(COMPARE_PREFIX) {
        return match origin.compares.get(tag) {
            Some(CompareAnswer::Status(status)) => {
                Json(json!({ "status": status })).into_response()
            }
            Some(CompareAnswer::Unreadable) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "compare unavailable").into_response()
            }
            None => (StatusCode::NOT_FOUND, "unknown tag").into_response(),
        };
    }
    (StatusCode::NOT_FOUND, "unknown path").into_response()
}

/// Bind a loopback listener and serve until the guard is dropped, including
/// when an assertion panics mid-test.
struct FixtureServer {
    origin: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(origin: ReleaseOrigin) -> FixtureServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available in a test process");
    let address = listener.local_addr().expect("a bound address");
    let app = Router::new().fallback(answer).with_state(Arc::new(origin));
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("the fixture server answers");
    });
    FixtureServer {
        origin: format!("http://{address}"),
        task,
    }
}

#[derive(Serialize)]
struct Case<'a> {
    name: &'a str,
    api: String,
    compare: String,
    channel: Option<&'a str>,
}

#[derive(Deserialize)]
struct Selection {
    name: String,
    selected: Option<String>,
}

/// A directory removed when the case finishes, so a failed harness leaves no
/// files behind on a developer's machine.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "roost-install-ps1-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("the scratch directory is created");
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Load every function declaration out of the real `install.ps1` and call the
/// public selection entrypoint once per case. Nothing at script top level
/// executes, which is what keeps the installation body out of the test.
const HARNESS: &str = r#"
param(
    [Parameter(Mandatory = $true)][string] $Installer,
    [Parameter(Mandatory = $true)][string] $Cases,
    [Parameter(Mandatory = $true)][string] $Output
)
$ErrorActionPreference = 'Stop'
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($Installer, [ref] $tokens, [ref] $parseErrors)
if ($parseErrors -and $parseErrors.Count -gt 0) {
    throw "install.ps1 does not parse: $($parseErrors[0].Message)"
}
$definitions = $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true)
if (-not ($definitions | Where-Object { $_.Name -eq 'Select-RoostTag' })) {
    throw 'install.ps1 defines no Select-RoostTag function'
}
foreach ($definition in $definitions) {
    . ([scriptblock]::Create($definition.Extent.Text))
}
$results = New-Object System.Collections.Generic.List[object]
foreach ($case in (Get-Content -Raw -LiteralPath $Cases | ConvertFrom-Json)) {
    $env:ROOST_RELEASE_COMPARE_URL = $case.compare
    $channel = $case.channel
    $selected = Select-RoostTag -Api $case.api -Channel $channel
    $results.Add([pscustomobject] @{ name = $case.name; selected = $selected })
}
$array = $results.ToArray()
$json = ConvertTo-Json -InputObject $array -Depth 4 -Compress
$utf8 = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($Output, $json, $utf8)
"#;

fn installer_script() -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.ps1");
    assert!(script.exists(), "install.ps1 exists at the repository root");
    script
}

/// Run Windows PowerShell 5.1 on the real script's selector for every case in
/// one invocation, and return the tag it chose per case.
async fn select(origin: &str, cases: &[Case<'_>], label: &str) -> BTreeMap<String, Option<String>> {
    let scratch = Scratch::new(label);
    let harness = scratch.path.join("select.ps1");
    let case_file = scratch.path.join("cases.json");
    let result_file = scratch.path.join("selected.json");
    std::fs::write(&harness, HARNESS).expect("the harness is written");
    std::fs::write(
        &case_file,
        serde_json::to_vec(cases).expect("the cases serialize"),
    )
    .expect("the cases are written");

    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new("powershell.exe")
            .kill_on_drop(true)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&harness)
            .arg("-Installer")
            .arg(installer_script())
            .arg("-Cases")
            .arg(&case_file)
            .arg("-Output")
            .arg(&result_file)
            .output(),
    )
    .await
    .expect("Windows PowerShell finished within two minutes")
    .expect("Windows PowerShell runs");

    assert!(
        output.status.success(),
        "powershell.exe failed for {origin}: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(&result_file).expect("the harness wrote its result");
    let text = text.trim_start_matches('\u{feff}');
    let selections: Vec<Selection> = serde_json::from_str(text).expect("the harness emitted JSON");
    selections
        .into_iter()
        .map(|selection| (selection.name, selection.selected))
        .collect()
}

fn chosen<'a>(selected: &'a BTreeMap<String, Option<String>>, name: &str) -> Option<&'a str> {
    selected
        .get(name)
        .unwrap_or_else(|| panic!("the harness returned no case {name:?}: {selected:?}"))
        .as_deref()
}

fn listed(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|tag| tag.to_string()).collect()
}

/// A side-branch tag can outrank every real release, so the selector must walk
/// past it. The same rule applies on the prerelease channel, to a stable tag
/// that would otherwise be the default channel's pick, and to the stable
/// channel, which must not fall through to a candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_windows_installer_takes_only_a_tag_whose_commit_is_on_v3() {
    let mut origin = ReleaseOrigin::default();
    origin.listings.insert(
        "/listing/prerelease".to_string(),
        listed(&["v3.0.0-rc.96", "v3.0.0-rc.win.4"]),
    );
    origin.listings.insert(
        "/listing/default".to_string(),
        listed(&["v3.99.0", "v3.0.0-rc.96"]),
    );
    origin.compares.insert(
        "v3.0.0-rc.win.4".to_string(),
        CompareAnswer::Status("ahead"),
    );
    origin
        .compares
        .insert("v3.0.0-rc.96".to_string(), CompareAnswer::Status("behind"));
    origin
        .compares
        .insert("v3.99.0".to_string(), CompareAnswer::Status("ahead"));

    let server = serve(origin).await;
    let url = &server.origin;
    let compare = format!("{url}/compare");
    let cases = [
        Case {
            name: "prerelease channel passes over an off-branch candidate",
            api: format!("{url}/listing/prerelease"),
            compare: compare.clone(),
            channel: Some("prerelease"),
        },
        Case {
            name: "an off-branch stable does not block the default fallback",
            api: format!("{url}/listing/default"),
            compare: compare.clone(),
            channel: None,
        },
        Case {
            name: "an explicit stable channel never falls back to a candidate",
            api: format!("{url}/listing/default"),
            compare,
            channel: Some("stable"),
        },
    ];
    let selected = select(url, &cases, "off-branch").await;

    assert_eq!(
        chosen(
            &selected,
            "prerelease channel passes over an off-branch candidate"
        ),
        Some("v3.0.0-rc.96"),
        "rc.win.4 sorts above rc.96 but its commit is not on v3: {selected:?}"
    );
    assert_eq!(
        chosen(
            &selected,
            "an off-branch stable does not block the default fallback"
        ),
        Some("v3.0.0-rc.96"),
        "the off-branch stable is skipped and the candidate is still installed: {selected:?}"
    );
    assert_eq!(
        chosen(
            &selected,
            "an explicit stable channel never falls back to a candidate"
        ),
        None,
        "stable asked for stable; a filtered candidate is not a substitute: {selected:?}"
    );
}

/// A compare that cannot be read — rate limit, mirror, network — admits the
/// candidate rather than making every install impossible. A readable `ahead`
/// is the only answer that rejects one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_windows_installer_admits_a_candidate_when_the_compare_answer_is_unreadable() {
    let mut origin = ReleaseOrigin::default();
    origin.listings.insert(
        "/listing/prerelease".to_string(),
        listed(&["v3.0.0-rc.5", "v3.0.0-rc.4"]),
    );
    origin.listings.insert(
        "/listing/default".to_string(),
        listed(&["v3.0.0", "v3.0.0-rc.9"]),
    );
    origin
        .compares
        .insert("v3.0.0-rc.5".to_string(), CompareAnswer::Unreadable);
    origin
        .compares
        .insert("v3.0.0-rc.4".to_string(), CompareAnswer::Status("behind"));
    origin
        .compares
        .insert("v3.0.0".to_string(), CompareAnswer::Unreadable);
    origin
        .compares
        .insert("v3.0.0-rc.9".to_string(), CompareAnswer::Status("behind"));

    let server = serve(origin).await;
    let url = &server.origin;
    let compare = format!("{url}/compare");
    let cases = [
        Case {
            name: "an unreadable compare admits the candidate on the prerelease channel",
            api: format!("{url}/listing/prerelease"),
            compare: compare.clone(),
            channel: Some("prerelease"),
        },
        Case {
            name: "an unreadable compare admits the stable on the default channel",
            api: format!("{url}/listing/default"),
            compare,
            channel: None,
        },
    ];
    let selected = select(url, &cases, "unreadable").await;

    assert_eq!(
        chosen(
            &selected,
            "an unreadable compare admits the candidate on the prerelease channel"
        ),
        Some("v3.0.0-rc.5"),
        "the newest candidate is installed when GitHub cannot answer: {selected:?}"
    );
    assert_eq!(
        chosen(
            &selected,
            "an unreadable compare admits the stable on the default channel"
        ),
        Some("v3.0.0"),
        "an unreadable answer must not demote a stable release to a candidate: {selected:?}"
    );
}
