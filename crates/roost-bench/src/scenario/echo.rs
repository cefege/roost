//! `echo_rtt`: 200 single keystrokes, each timed from the moment the probe is
//! armed to the paint that shows the echoed character. The probe counts `a`
//! on screen rather than searching for a run of them, because the prompt
//! itself (`bash-5.2$`) already contains one.

use std::time::Duration;

use serde_json::json;

use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage};

const KEYSTROKES: usize = 200;
/// Clear the typed line this often, so it never wraps.
const LINE_RESET_EVERY: usize = 40;
const ECHO_DEADLINE: Duration = Duration::from_secs(5);
const LETTER: char = 'a';

pub async fn echo_rtt(session: &SessionPage<'_>) -> Result<Vec<Sample>, BenchError> {
    let page = &session.page;
    session.shell_sync().await?;
    let baseline = page.count_char(LETTER).await?;
    let mut samples = Vec::with_capacity(KEYSTROKES);
    for stroke in 0..KEYSTROKES {
        let typed = stroke % LINE_RESET_EVERY;
        if typed == 0 && stroke > 0 {
            page.arm_count(LETTER, baseline).await?;
            page.press_ctrl_u().await?;
            page.await_armed(ECHO_DEADLINE).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let armed_at = page.arm_count(LETTER, baseline + typed + 1).await?;
        page.press_letter(LETTER).await?;
        let painted_at = page.await_armed(ECHO_DEADLINE).await?;
        samples.push(Sample::new(
            "echo_rtt_ms",
            painted_at - armed_at,
            json!({ "stroke": stroke + 1 }),
        ));
    }
    page.arm_count(LETTER, baseline).await?;
    page.press_ctrl_u().await?;
    page.await_armed(ECHO_DEADLINE).await?;
    Ok(samples)
}
