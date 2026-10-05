//! `fanout`: four pages view one session; one 20 000-line flood typed on the
//! first is timed on every page. `fanout_p1_ms` is the typing page,
//! `fanout_max_ms` the slowest of the four.

use std::time::Duration;

use futures::future::join_all;
use serde_json::json;

use crate::browser::BenchPage;
use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage, wait_session_ready};

const EXTRA_VIEWERS: usize = 3;
const FLOOD_DEADLINE: Duration = Duration::from_secs(120);
const FLOOD_COMMAND: &str = "seq 1 20000";

pub async fn fanout(session: &SessionPage<'_>) -> Result<Vec<Sample>, BenchError> {
    let mut viewers: Vec<BenchPage> = Vec::with_capacity(EXTRA_VIEWERS);
    let measured = async {
        for _ in 0..EXTRA_VIEWERS {
            let viewer = session.browser.open_page(&session.session_url).await?;
            viewers.push(viewer);
        }
        for viewer in &viewers {
            wait_session_ready(viewer).await?;
        }
        measure(session, &viewers).await
    }
    .await;
    for viewer in viewers {
        viewer.close().await;
    }
    measured
}

async fn measure(
    session: &SessionPage<'_>,
    viewers: &[BenchPage],
) -> Result<Vec<Sample>, BenchError> {
    let typing = &session.page;
    typing.focus_terminal().await?;
    session.shell_sync().await?;
    typing
        .type_text(&format!("{FLOOD_COMMAND}; echo DONE-$X-f"))
        .await?;
    let needle = format!("DONE-{}-f", session.nonce);
    for viewer in viewers {
        viewer.arm_text(&needle).await?;
    }
    let started_at = typing.arm_text(&needle).await?;
    typing.press_enter().await?;
    let pages = std::iter::once(typing).chain(viewers.iter());
    let painted: Vec<Result<f64, BenchError>> =
        join_all(pages.map(|page| page.await_armed(FLOOD_DEADLINE))).await;
    let mut painted_at = Vec::with_capacity(painted.len());
    for result in painted {
        painted_at.push(result?);
    }
    let typing_ms = painted_at.first().copied().unwrap_or(f64::NAN) - started_at;
    let slowest_ms = painted_at.iter().copied().fold(f64::MIN, f64::max) - started_at;
    let per_page: Vec<f64> = painted_at.iter().map(|at| at - started_at).collect();
    Ok(vec![
        Sample::new("fanout_p1_ms", typing_ms, json!({ "perPageMs": per_page })),
        Sample::new(
            "fanout_max_ms",
            slowest_ms,
            json!({ "viewers": viewers.len() + 1 }),
        ),
    ])
}
