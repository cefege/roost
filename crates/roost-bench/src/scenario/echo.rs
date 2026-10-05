//! `echo_rtt`: 200 single keystrokes, each timed from the moment the probe is
//! armed to the paint that shows the echoed character. The probe counts `a`
//! on screen rather than searching for a run of them, because the prompt
//! itself (`bash-5.2$`) already contains one.
//!
//! A CDP socket trace splits each echo at the frame that produced the paint.
//! `echo_reply_ms` is the key frame sent → that frame handled by the page;
//! Chromium stamps a received frame when the renderer's main thread runs it,
//! so this includes the wait behind the frame the keystroke itself scheduled,
//! not only the server. `echo_paint_ms` is that frame → the paint.

use std::time::Duration;

use serde_json::json;

use crate::browser::{TraceEvent, TraceKind};
use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage, carrier_of};

const KEYSTROKES: usize = 200;
/// Clear the typed line this often, so it never wraps.
const LINE_RESET_EVERY: usize = 40;
const ECHO_DEADLINE: Duration = Duration::from_secs(5);
/// CDP's network events and the evaluation that saw the paint travel apart,
/// so a stroke is split only once the next one starts; the last one waits
/// this long instead. A sleep inside the loop would move every keystroke's
/// phase against the display frame, which is what the echo time depends on.
const TRACE_SETTLE: Duration = Duration::from_millis(20);
const LETTER: char = 'a';

/// One timed keystroke, split once its trace events are in.
struct Stroke {
    number: usize,
    armed_at: f64,
    painted_at: f64,
}

pub async fn echo_rtt(session: &SessionPage<'_>) -> Result<Vec<Sample>, BenchError> {
    let page = &session.page;
    session.shell_sync().await?;
    let baseline = page.count_char(LETTER).await?;
    let trace = page.start_socket_trace().await?;
    let mut samples = Vec::with_capacity(KEYSTROKES * 3);
    let mut carrier = Some(carrier_of(page).await);
    let mut previous: Option<Stroke> = None;
    for stroke in 0..KEYSTROKES {
        if let Some(done) = previous.take() {
            samples.extend(split_echo(&done, &trace.drain()));
        }
        let typed = stroke % LINE_RESET_EVERY;
        if typed == 0 && stroke > 0 {
            page.arm_count(LETTER, baseline).await?;
            page.press_ctrl_u().await?;
            page.await_armed(ECHO_DEADLINE).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        trace.drain();
        let armed_at = page.arm_count(LETTER, baseline + typed + 1).await?;
        page.press_letter(LETTER).await?;
        let painted_at = page.await_armed(ECHO_DEADLINE).await?;
        let mut rtt_detail = json!({ "stroke": stroke + 1 });
        if let (Some(carrier), Some(fields)) = (carrier.take(), rtt_detail.as_object_mut()) {
            fields.insert("carrier".to_string(), carrier);
        }
        samples.push(Sample::new(
            "echo_rtt_ms",
            painted_at - armed_at,
            rtt_detail,
        ));
        previous = Some(Stroke {
            number: stroke + 1,
            armed_at,
            painted_at,
        });
    }
    if let Some(done) = previous {
        tokio::time::sleep(TRACE_SETTLE).await;
        samples.extend(split_echo(&done, &trace.drain()));
    }
    drop(trace);
    page.arm_count(LETTER, baseline).await?;
    page.press_ctrl_u().await?;
    page.await_armed(ECHO_DEADLINE).await?;
    Ok(samples)
}

/// The reply and paint halves of one echo. The paint came from the last frame
/// received before it; the first one received is the input acknowledgement.
/// A stroke whose key frame was not seen on a WebSocket (input took another
/// carrier) records NaN, which the summaries drop.
fn split_echo(stroke: &Stroke, events: &[TraceEvent]) -> [Sample; 2] {
    let key_sent = events
        .iter()
        .find(|event| event.kind == TraceKind::Sent)
        .map(|event| event.epoch_ms);
    let received: Vec<&TraceEvent> = events
        .iter()
        .filter(|event| event.kind == TraceKind::Received && event.epoch_ms <= stroke.painted_at)
        .collect();
    let first_reply = received.first().map(|event| event.epoch_ms);
    let painted_reply = received.last().map(|event| event.epoch_ms);
    let (Some(key_sent), Some(painted_reply)) = (key_sent, painted_reply) else {
        let missing = if key_sent.is_none() {
            "key_sent"
        } else {
            "reply"
        };
        let detail = json!({ "stroke": stroke.number, "missing": missing });
        return [
            Sample::new("echo_reply_ms", f64::NAN, detail.clone()),
            Sample::new("echo_paint_ms", f64::NAN, detail),
        ];
    };
    let detail = json!({
        "stroke": stroke.number,
        "key_sent_epoch_ms": key_sent,
        "key_sent_rel": key_sent - stroke.armed_at,
        "first_reply_rel": first_reply.map(|at| at - stroke.armed_at),
        "painted_reply_rel": painted_reply - stroke.armed_at,
        "reply_bytes": received.iter().map(|event| event.bytes).collect::<Vec<_>>(),
    });
    [
        Sample::new("echo_reply_ms", painted_reply - key_sent, detail.clone()),
        Sample::new("echo_paint_ms", stroke.painted_at - painted_reply, detail),
    ]
}
