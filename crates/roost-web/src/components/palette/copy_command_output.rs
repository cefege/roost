//! "Copy last command output": the rows between the newest finished command's
//! OSC 133 output mark and the prompt that followed it, as text.
//!
//! Called by `palette::outcome`. Reads the session's live frame first and pages
//! retained history through the existing `SessionsGetScrollbackCells` read only
//! when the output started above the screen, without painting those pages.

use roost_protocol::cell::{CellRow, row_mark};

#[cfg(target_arch = "wasm32")]
use crate::pump::Pump;

/// The most text one copy carries; the rest of a longer output is cut.
pub const COPY_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

/// How far back history is paged before giving up on finding a finished
/// command: the worker's whole retained scrollback (`roost_term::SCROLLBACK_LINES`).
#[cfg(target_arch = "wasm32")]
const COPY_OUTPUT_MAX_ROWS: u64 = 10_000;

/// Rows asked for per history page.
#[cfg(target_arch = "wasm32")]
const COPY_OUTPUT_PAGE_ROWS: u32 = 500;

/// The newest finished command's output rows, as `[output_start, next_prompt)`
/// over `rows`, which must be contiguous and sorted oldest first.
///
/// A command is finished once a prompt follows its output mark. An empty Enter
/// leaves a prompt with no output mark behind it and is skipped, so the answer
/// is the newest command that actually ran. A mark row that is also the next
/// prompt's row is a command that printed nothing: an empty range.
#[must_use]
pub fn last_finished_output(rows: &[CellRow]) -> Option<(u32, u32)> {
    let mut next_prompt: Option<u32> = None;
    for row in rows.iter().rev() {
        if row.mark & row_mark::OUTPUT != 0 {
            if row.mark & row_mark::PROMPT != 0 {
                return Some((row.index, row.index));
            }
            if let Some(prompt) = next_prompt {
                return Some((row.index, prompt));
            }
        }
        if row.mark & row_mark::PROMPT != 0 {
            next_prompt = Some(row.index);
        }
    }
    None
}

/// The text of `[start, end)`: one line per row, trailing blank rows dropped,
/// cut at [`COPY_OUTPUT_MAX_BYTES`] on a character boundary.
#[must_use]
pub fn output_text(rows: &[CellRow], start: u32, end: u32) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .filter(|row| row.index >= start && row.index < end)
        .map(|row| roost_protocol::cell::spans_text(&row.spans))
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let mut text = lines.join("\n");
    if text.len() > COPY_OUTPUT_MAX_BYTES {
        let mut boundary = COPY_OUTPUT_MAX_BYTES;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
    }
    text
}

/// Find, read and copy the newest finished command's output.
pub fn copy_last_command_output(pump: crate::pump::Pump, session_id: String) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let outcome = read_last_output(&pump, &session_id).await;
        report(&pump, &session_id, outcome);
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, session_id);
}

/// The live frame's rows, then older history pages until a finished command
/// shows up or the retained history runs out.
#[cfg(target_arch = "wasm32")]
async fn read_last_output(pump: &Pump, session_id: &str) -> Result<String, String> {
    use roost_client_core::client::rpc::calls::terminal_pane::ScrollbackCells;

    let (grid_epoch, scrollback_total, mut rows) = {
        let core = pump.core();
        let core = core.borrow();
        let frame = core
            .store()
            .terminal(session_id)
            .and_then(|replica| replica.canonical())
            .ok_or_else(|| "this terminal is not showing yet".to_owned())?;
        let base = u32::try_from(frame.scrollback_total).unwrap_or(u32::MAX);
        let viewport: Vec<CellRow> = frame
            .viewport_rows
            .iter()
            .map(|row| CellRow {
                index: base.saturating_add(row.index),
                ..row.clone()
            })
            .collect();
        (frame.grid_epoch.clone(), frame.scrollback_total, viewport)
    };
    let mut oldest = scrollback_total;
    loop {
        if let Some((start, end)) = last_finished_output(&rows) {
            return Ok(output_text(&rows, start, end));
        }
        if oldest == 0 || scrollback_total - oldest >= COPY_OUTPUT_MAX_ROWS {
            return Err("no finished command in this terminal's history".to_owned());
        }
        let page = pump
            .rpc()
            .call(&ScrollbackCells {
                session_id: session_id.to_owned(),
                end_row: oldest,
                max_rows: COPY_OUTPUT_PAGE_ROWS,
                grid_epoch: grid_epoch.clone(),
            })
            .await
            .map_err(|error| error.to_string())?;
        if page.grid_epoch != grid_epoch {
            return Err("the terminal was redrawn while reading it; try again".to_owned());
        }
        if page.rows.is_empty() || page.start_row >= oldest {
            return Err("no finished command in this terminal's history".to_owned());
        }
        oldest = page.start_row;
        rows.splice(0..0, page.rows);
    }
}

/// Copy the text, or say why there is nothing to copy. The write lands after
/// an await, outside the palette press, so a browser that refuses it gets the
/// Copy card whose click is the gesture it wants.
#[cfg(target_arch = "wasm32")]
fn report(pump: &Pump, session_id: &str, outcome: Result<String, String>) {
    use roost_client_core::ClientEvent;
    use roost_client_core::store::shell_intent::ShellIntent;

    let text = match outcome {
        Ok(text) if text.is_empty() => {
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                message: "The last command printed nothing".to_owned(),
            }));
            return;
        }
        Ok(text) => text,
        Err(reason) => {
            tracing::info!(target: "palette", %session_id, %reason, "copy last command output failed");
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                message: format!("Could not copy the command output: {reason}"),
            }));
            return;
        }
    };
    let lines = text.lines().count();
    let pump = pump.clone();
    let session_id = session_id.to_owned();
    let fallback = text.clone();
    crate::components::notifications::clipboard::copy_text_then(&text, move |copied| {
        if copied {
            let call = roost_client_core::client::rpc::calls::clipboard::ClipboardAdd {
                text: fallback.clone(),
                session_id: session_id.clone(),
                source_kind: "command_output".to_owned(),
            };
            let rpc = pump.rpc();
            let session_id = session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(error) = rpc.call(&call).await {
                    tracing::debug!(target: "clipboard", %session_id, %error, "command output history add failed");
                }
            });
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                message: format!("Copied {lines} lines of command output"),
            }));
        } else {
            crate::components::notifications::terminal_clipboard::raise_manual_copy_toast(
                &pump,
                &session_id,
                "The command output is ready. Click Copy to put it on your clipboard.",
                fallback,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use roost_protocol::cell::{CellRow, CellSpan, DEFAULT_COLOR, row_mark};

    use super::{last_finished_output, output_text};

    fn row(index: u32, mark: u8, text: &str) -> CellRow {
        CellRow {
            index,
            mark,
            spans: Arc::from(vec![CellSpan {
                text: text.to_owned(),
                fg: DEFAULT_COLOR,
                bg: DEFAULT_COLOR,
                flags: 0,
                fg_rgb: None,
                bg_rgb: None,
                columns: text.chars().count().max(1) as u32,
                link_uri: None,
                link_key: None,
            }]),
        }
    }

    #[test]
    fn the_newest_command_with_output_is_chosen_over_a_later_empty_enter() {
        let rows = [
            row(10, row_mark::PROMPT, "$ make"),
            row(11, row_mark::OUTPUT, "compiling"),
            row(12, 0, "done"),
            row(13, row_mark::PROMPT | row_mark::EXIT_OK, "$"),
            row(14, row_mark::PROMPT, "$"),
        ];
        assert_eq!(last_finished_output(&rows), Some((11, 13)));
        assert_eq!(output_text(&rows, 11, 13), "compiling\ndone");
    }

    #[test]
    fn a_command_still_running_is_not_finished() {
        let rows = [
            row(0, row_mark::PROMPT, "$ sleep 60"),
            row(1, row_mark::OUTPUT, ""),
        ];
        assert_eq!(last_finished_output(&rows), None);
    }

    #[test]
    fn a_command_that_printed_nothing_is_an_empty_range() {
        let rows = [
            row(0, row_mark::PROMPT, "$ true"),
            row(
                1,
                row_mark::OUTPUT | row_mark::PROMPT | row_mark::EXIT_OK,
                "$",
            ),
        ];
        assert_eq!(last_finished_output(&rows), Some((1, 1)));
        assert_eq!(output_text(&rows, 1, 1), "");
    }
}
