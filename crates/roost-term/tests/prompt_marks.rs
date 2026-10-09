//! OSC 133 marks land on the rows the shell emitted them on, in stream order,
//! and a finished command's status is carried to the NEXT prompt. These marks
//! are what the browser's prompt gutter, prompt jump and "copy last output"
//! read, so a mark on the wrong row is a bar beside the wrong command.

use roost_protocol::cell::row_mark;
use roost_term::TerminalCore;
use roost_term::frame::grid_to_cell_frame;
use roost_term::rio::RioCore;

fn frame(core: &RioCore) -> roost_protocol::cell::CellGridFrame {
    let discarded = core.discarded_line_count().unwrap_or(0);
    grid_to_cell_frame(core, 1, "grid:0", "stream", None, discarded)
}

fn marks(core: &RioCore) -> Vec<u8> {
    frame(core)
        .viewport_rows
        .iter()
        .map(|row| row.mark)
        .collect()
}

/// What a bash or zsh shell with Roost's hooks prints around one command:
/// prompt, the typed command echoed and Enter, `C` from PS0/preexec, output,
/// then `D;<status>` and the next prompt's `A`.
fn command(status: u8) -> Vec<u8> {
    format!(
        "\x1b]133;A\x07$ make\r\n\x1b]133;C\x07output\r\n\x1b]133;D;{status}\x07\x1b]133;A\x07$ "
    )
    .into_bytes()
}

#[test]
fn a_finished_command_colours_the_prompt_that_follows_it() {
    let mut core = RioCore::new(40, 4);
    core.write_raw(&command(0));
    assert_eq!(
        marks(&core),
        [
            row_mark::PROMPT,
            row_mark::OUTPUT,
            row_mark::PROMPT | row_mark::EXIT_OK,
            0
        ]
    );

    let mut failed = RioCore::new(40, 4);
    failed.write_raw(&command(2));
    assert_eq!(marks(&failed)[2], row_mark::PROMPT | row_mark::EXIT_FAILED);
}

#[test]
fn a_status_with_no_command_behind_it_colours_nothing() {
    let mut core = RioCore::new(40, 4);
    // The first prompt, then an empty Enter: D arrives with no C before it.
    core.write_raw(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \r\n\x1b]133;D;1\x07\x1b]133;A\x07$ ");
    assert_eq!(marks(&core)[..2], [row_mark::PROMPT, row_mark::PROMPT]);
}

#[test]
fn the_status_survives_a_blank_line_before_the_next_prompt() {
    // starship's default `add_newline` prints a blank line between D and A.
    let mut core = RioCore::new(40, 5);
    core.write_raw(b"\x1b]133;A\x07$ false\r\n\x1b]133;C\x07\x1b]133;D;1\x07\r\n\x1b]133;A\x07$ ");
    assert_eq!(
        marks(&core)[..3],
        [
            row_mark::PROMPT,
            row_mark::OUTPUT,
            row_mark::PROMPT | row_mark::EXIT_FAILED
        ]
    );
}

#[test]
fn a_mark_split_across_chunks_lands_where_it_ends_and_replay_rebuilds_it() {
    let mut live = RioCore::new(40, 4);
    live.write_raw(b"before\r\n\x1b]13");
    live.write_raw(b"3;A\x07prompt");
    assert_eq!(marks(&live)[..2], [0, row_mark::PROMPT]);

    let mut replay = RioCore::new(40, 4);
    replay.write(&command(1));
    assert_eq!(
        marks(&replay),
        marks(&{
            let mut again = RioCore::new(40, 4);
            again.write_raw(&command(1));
            again
        })
    );
}

#[test]
fn the_prompt_mark_survives_the_prompt_being_drawn_over_its_cell() {
    let mut core = RioCore::new(40, 4);
    // The prompt is drawn after A, then redrawn in place (zle reset-prompt).
    core.write_raw(b"\x1b]133;A\x07~/src $ \r~/src/roost $ ");
    assert_eq!(marks(&core)[0], row_mark::PROMPT);
}

#[test]
fn the_alternate_screen_takes_no_marks_and_clearing_erases_them() {
    let mut core = RioCore::new(40, 4);
    core.write_raw(b"\x1b[?1049h\x1b]133;A\x07\x1b]133;C\x07output\r\n");
    assert!(marks(&core).iter().all(|mark| *mark == 0));

    core.write_raw(b"\x1b[?1049l\x1b]133;A\x07prompt\x1b[H\x1b[2J");
    assert!(marks(&core).iter().all(|mark| *mark == 0));
}
