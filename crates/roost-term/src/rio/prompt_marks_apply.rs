//! Apply OSC 133 marks to the grid row under the cursor.
//!
//! `super::RioCore::parse` splits the byte stream at each mark and calls
//! [`RioCore::apply_prompt_mark`] with the emulator positioned exactly where
//! the shell emitted it; the row keeps the mark through the prompt being drawn
//! over it and into history (`Row::roost_mark`, vendored patch R5).
//!
//! WHY THE EXIT STATUS RIDES THE NEXT PROMPT. `D` arrives after the command's
//! output, so the prompt the command was typed at is usually already in
//! history, and a retained history row is something a delta cannot replace —
//! stamping it there would need a full frame per finished command and would
//! still leave the browser's painted copy stale. The status is held until the
//! next `A` and stamped on that prompt instead, which is also where starship and
//! powerlevel10k show the previous command's status: on the prompt that follows.

use roost_protocol::cell::row_mark;

use super::RioCore;
use super::prompt_marks::PromptMark;
use crate::core::{CommandEvent, TerminalCore};

/// Where the command lifecycle between two prompts stands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandLifecycle {
    /// `C` arrived since the last `A`: a command really ran.
    started: bool,
    /// The exit bit a finished command left for the next prompt.
    pending_exit: Option<u8>,
}

impl RioCore {
    pub(super) fn apply_prompt_mark(&mut self, mark: PromptMark) {
        // The alternate screen is a full-screen program's canvas, never a shell
        // prompt, and its rows are discarded when the program exits.
        if self.using_alt_screen() {
            return;
        }
        match mark {
            PromptMark::Prompt => {
                let exit = self.command_lifecycle.pending_exit.take().unwrap_or(0);
                self.mark_cursor_row(row_mark::PROMPT | exit);
                self.command_lifecycle.started = false;
            }
            PromptMark::Output => {
                self.mark_cursor_row(row_mark::OUTPUT);
                if !self.command_lifecycle.started {
                    self.command_events.push_back(CommandEvent::Started);
                    self.command_lifecycle.started = true;
                }
            }
            // A `D` with no `C` before it is an empty Enter or the shell's very
            // first prompt: no command ran, so there is no status to show.
            PromptMark::Finished(exit_code) => {
                if std::mem::take(&mut self.command_lifecycle.started) {
                    self.command_events
                        .push_back(CommandEvent::Finished { exit_code });
                    self.command_lifecycle.pending_exit = Some(if exit_code == 0 {
                        row_mark::EXIT_OK
                    } else {
                        row_mark::EXIT_FAILED
                    });
                }
            }
        }
    }

    /// OR `bits` into the cursor row's mark.
    fn mark_cursor_row(&mut self, bits: u8) {
        let line = self.term.grid.cursor.pos.row;
        let row = &mut self.term.grid[line];
        row.roost_mark |= bits;
        let row = u16::try_from(line.0).unwrap_or(0);
        if !self.dirty.contains(&row) {
            self.dirty.push(row);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::super::RioCore;
    use crate::core::{CommandEvent, TerminalCore};

    #[test]
    fn live_osc_133_command_lifecycle_is_one_shot_and_carries_exit_code() {
        let mut core = RioCore::new(80, 24);
        core.write_raw(b"\x1b]133;C\x07command output");
        assert_eq!(core.take_command_events(), vec![CommandEvent::Started]);

        core.write_raw(b"\x1b]133;D;17\x07");
        assert_eq!(
            core.take_command_events(),
            vec![CommandEvent::Finished { exit_code: 17 }]
        );
        assert!(core.take_command_events().is_empty());
    }

    #[test]
    fn replay_discards_command_events() {
        let mut core = RioCore::new(80, 24);
        core.write(b"\x1b]133;C\x07\x1b]133;D;0\x07");
        assert!(core.take_command_events().is_empty());
    }

    #[test]
    fn live_bell_is_emitted_once_and_replay_discards_it() {
        let mut core = RioCore::new(80, 24);
        core.write_raw(b"\x07\x07");
        assert_eq!(core.take_bell_events(), 2);
        assert_eq!(core.take_bell_events(), 0);

        core.write(b"\x07");
        assert_eq!(core.take_bell_events(), 0);
    }
}
