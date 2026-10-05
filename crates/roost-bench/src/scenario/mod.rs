//! The measured workloads, each returning samples in milliseconds. They share
//! one session page per round (`SessionPage`) and one shell, whose `X` variable
//! holds a per-round nonce: every marker the harness waits for is built from
//! `$X`, so the echoed command line can never match its own marker.

mod cold_nav;
mod echo;
mod fanout;
mod flood;
mod startup;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::browser::{BenchBrowser, BenchPage};
use crate::error::BenchError;

pub use cold_nav::cold_nav;
pub use echo::echo_rtt;
pub use fanout::fanout;
pub use flood::{FloodKind, flood};
pub use startup::startup_samples;

const PAGE_READY_DEADLINE: Duration = Duration::from_secs(30);
const SHELL_SYNC_DEADLINE: Duration = Duration::from_secs(30);

/// One measurement.
#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub metric: &'static str,
    pub value_ms: f64,
    pub detail: Value,
}

impl Sample {
    pub fn new(metric: &'static str, value_ms: f64, detail: Value) -> Self {
        Self {
            metric,
            value_ms,
            detail,
        }
    }
}

/// The session every scenario of a round drives.
#[derive(Debug)]
pub struct SessionPage<'browser> {
    pub browser: &'browser BenchBrowser,
    pub page: BenchPage,
    pub session_url: String,
    pub nonce: String,
    sync_counter: AtomicU32,
}

impl<'browser> SessionPage<'browser> {
    pub fn new(
        browser: &'browser BenchBrowser,
        page: BenchPage,
        session_url: String,
    ) -> Result<Self, BenchError> {
        Ok(Self {
            browser,
            page,
            session_url,
            nonce: random_hex()?,
            sync_counter: AtomicU32::new(0),
        })
    }

    /// Clear the screen and wait until the shell answered: the screen then
    /// holds one marker line and a fresh prompt.
    pub async fn shell_sync(&self) -> Result<(), BenchError> {
        let step = self.sync_counter.fetch_add(1, Ordering::Relaxed);
        self.page
            .submit_line(&format!("clear; echo R-$X-{step}"))
            .await?;
        self.page
            .wait_for_text(&format!("R-{}-{step}", self.nonce), SHELL_SYNC_DEADLINE)
            .await?;
        // The prompt follows the marker by a few ms; scenarios read the screen
        // after this, so let it land.
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(())
    }

    /// Put the nonce in the shell. Run once the page shows a prompt.
    pub async fn install_nonce(&self) -> Result<(), BenchError> {
        self.page.focus_terminal().await?;
        self.page
            .submit_line(&format!("X={}; echo R-$X-init", self.nonce))
            .await?;
        self.page
            .wait_for_text(&format!("R-{}-init", self.nonce), SHELL_SYNC_DEADLINE)
            .await?;
        Ok(())
    }
}

/// Fail unless the grid shows `needle`: a marker that painted without the
/// output before it would time something other than the flood.
pub async fn require_on_screen(page: &BenchPage, needle: &str) -> Result<(), BenchError> {
    let present: bool = page
        .eval(&format!(
            "window.__bench.gridText().includes({})",
            serde_json::Value::String(needle.to_string())
        ))
        .await?;
    if present {
        Ok(())
    } else {
        Err(BenchError::Browser(format!(
            "the marker painted but `{needle}` is not on screen"
        )))
    }
}
/// Wait until a session page painted cells and shows a prompt.
pub async fn wait_session_ready(page: &BenchPage) -> Result<(), BenchError> {
    page.poll_until(
        "first terminal cells",
        PAGE_READY_DEADLINE,
        "window.__bench && window.__bench.firstCellsEpochMs !== null",
    )
    .await?;
    page.wait_for_text("$", PAGE_READY_DEADLINE).await?;
    Ok(())
}

/// What the page reports about its terminal carrier, for the report.
pub async fn carrier_of(page: &BenchPage) -> Value {
    match page.eval::<String>("window.__bench.carrier()").await {
        Ok(text) if text.is_empty() => Value::String(String::new()),
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Err(error) => Value::String(format!("unreadable: {error}")),
    }
}

/// Eight lowercase hex characters from `/dev/urandom`.
fn random_hex() -> Result<String, BenchError> {
    use std::io::Read as _;
    let mut bytes = [0_u8; 4];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| BenchError::io("reading /dev/urandom", error))?;
    Ok(hex::encode(bytes))
}
