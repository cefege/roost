//! `flood_plain` and `flood_styled`: one command that writes a lot of output,
//! timed from Enter to the paint that shows the trailing marker. Three
//! repetitions each, the screen cleared between them.

use std::time::Duration;

use serde_json::json;

use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage, carrier_of, require_on_screen};

const REPETITIONS: usize = 3;
const FLOOD_DEADLINE: Duration = Duration::from_secs(120);

/// The output shapes measured.
#[derive(Debug, Clone, Copy)]
pub enum FloodKind {
    /// 20 000 short plain lines.
    Plain,
    /// 5 000 lines, each with a 256-colour foreground, bold, and a reset.
    Styled,
}

impl FloodKind {
    pub fn metric(self) -> &'static str {
        match self {
            Self::Plain => "flood_plain_ms",
            Self::Styled => "flood_styled_ms",
        }
    }

    /// The command line, minus its marker suffix.
    pub fn command(self) -> &'static str {
        match self {
            Self::Plain => "seq 1 20000",
            Self::Styled => {
                r#"awk 'BEGIN{for(i=1;i<=5000;i++)printf "\033[38;5;%dm%6d \033[1mbold\033[0m plain\n",i%256,i}'"#
            }
        }
    }

    /// Text of the flood's final line, which must be on screen with the marker.
    pub fn last_line(self) -> &'static str {
        match self {
            Self::Plain => "20000",
            Self::Styled => "5000",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Plain => "p",
            Self::Styled => "s",
        }
    }
}

pub async fn flood(session: &SessionPage<'_>, kind: FloodKind) -> Result<Vec<Sample>, BenchError> {
    let page = &session.page;
    let mut samples = Vec::with_capacity(REPETITIONS);
    for repetition in 1..=REPETITIONS {
        session.shell_sync().await?;
        page.eval::<serde_json::Value>("window.__bench.resetLongTasks()")
            .await?;
        let tag = format!("{}{repetition}", kind.tag());
        page.type_text(&format!("{}; echo DONE-$X-{tag}", kind.command()))
            .await?;
        let started_at = page
            .arm_text(&format!("DONE-{}-{tag}", session.nonce))
            .await?;
        page.press_enter().await?;
        let painted_at = page.await_armed(FLOOD_DEADLINE).await?;
        require_on_screen(page, kind.last_line()).await?;
        let long_tasks: serde_json::Value = page.eval("window.__bench.longTasks").await?;
        let rows: usize = page.eval("window.__bench.rowCount()").await?;
        samples.push(Sample::new(
            kind.metric(),
            painted_at - started_at,
            json!({
                "repetition": repetition,
                "longTasks": long_tasks,
                "carrier": carrier_of(page).await,
                "rows": rows,
            }),
        ));
    }
    Ok(samples)
}
