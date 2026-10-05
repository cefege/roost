//! One headless Chromium per (stack, round), driven over CDP by chromiumoxide.
//! Called by `run` and the scenarios; owns the probe script every page gets
//! (`probe.js`) and the pairing ceremony that enrolls the browser's own key.

mod page;
mod socket_trace;

use std::path::Path;
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt as _;
use tokio::task::JoinHandle;

use crate::error::BenchError;
use crate::stack::StackId;

pub use page::BenchPage;
pub use socket_trace::{TraceEvent, TraceKind};

const PROBE_JS: &str = include_str!("probe.js");
/// Long enough for the slowest awaited promise (a 120 s flood marker): a CDP
/// call that outlived this would fail before the page answered.
const CDP_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const PAIRED_DEADLINE: Duration = Duration::from_secs(30);

/// A launched Chromium and the task pumping its CDP connection.
#[derive(Debug)]
pub struct BenchBrowser {
    browser: Browser,
    pump: JoinHandle<()>,
}

impl BenchBrowser {
    /// `env` is added to Chromium's environment; it carries the round marker
    /// so the sampler attributes the renderer to this round.
    pub async fn launch(
        chrome: &Path,
        profile_dir: &Path,
        env: Vec<(String, String)>,
    ) -> Result<Self, BenchError> {
        let config = BrowserConfig::builder()
            .chrome_executable(chrome)
            .user_data_dir(profile_dir)
            .new_headless_mode()
            .window_size(1280, 800)
            .viewport(None)
            .request_timeout(CDP_REQUEST_TIMEOUT)
            .arg("disable-gpu")
            .envs(env)
            .build()
            .map_err(BenchError::Browser)?;
        let (browser, mut handler) = Browser::launch(config).await.map_err(BenchError::browser)?;
        let pump = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(error) = event {
                    tracing::debug!(%error, "cdp handler event error");
                }
            }
        });
        tracing::info!(profile = %profile_dir.display(), "chromium launched");
        Ok(Self { browser, pump })
    }

    /// A fresh tab with the probe installed, navigated to `url`. Each tab gets
    /// its own window: a background tab is hidden, and a hidden page stops
    /// painting, which would stall every fan-out viewer but the front one.
    pub async fn open_page(&self, url: &str) -> Result<BenchPage, BenchError> {
        let target = CreateTargetParams::builder()
            .url("about:blank")
            .new_window(true)
            .build()
            .map_err(BenchError::Browser)?;
        let page = self
            .browser
            .new_page(target)
            .await
            .map_err(BenchError::browser)?;
        page.evaluate_on_new_document(AddScriptToEvaluateOnNewDocumentParams::new(PROBE_JS))
            .await
            .map_err(BenchError::browser)?;
        let page = BenchPage::new(page);
        page.goto(url).await?;
        Ok(page)
    }

    pub async fn close(mut self) {
        if let Err(error) = self.browser.close().await {
            tracing::warn!(%error, "chromium close failed; killing it");
            self.browser.kill().await;
        }
        if let Err(error) = self.browser.wait().await {
            tracing::warn!(%error, "waiting for chromium to exit failed");
        }
        self.pump.abort();
    }
}

/// Enroll this browser through a one-shot bootstrap token and wait until the
/// app renders as a paired client. Returns the page, left on `/`.
pub async fn pair_browser(
    browser: &BenchBrowser,
    stack: StackId,
    coord_url: &str,
    token: &str,
) -> Result<BenchPage, BenchError> {
    let pair_url = match stack {
        StackId::V3 => format!("{coord_url}/pair#pair={token}"),
        StackId::V2 => format!("{coord_url}/#pair={token}"),
    };
    let page = browser.open_page(&pair_url).await?;
    let started = Instant::now();
    // The app scrubs the fragment when it captures the token, then redeems and
    // reloads; navigating away before that would abort the redeem.
    page.poll_until(
        "pair fragment scrubbed",
        PAIRED_DEADLINE,
        "!location.hash.includes('pair=')",
    )
    .await?;
    while started.elapsed() < PAIRED_DEADLINE {
        tokio::time::sleep(Duration::from_millis(500)).await;
        page.goto(&format!("{coord_url}/")).await?;
        let shell_drawn = page
            .poll_until(
                "paired shell",
                Duration::from_secs(5),
                "document.querySelector(\".wterm, aside[data-testid='sidebar-desktop']\") !== null",
            )
            .await;
        if shell_drawn.is_ok() {
            tracing::info!(
                stack = stack.as_str(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "browser paired"
            );
            return Ok(page);
        }
    }
    Err(BenchError::timeout(
        format!("{} browser pairing", stack.as_str()),
        PAIRED_DEADLINE,
    ))
}
