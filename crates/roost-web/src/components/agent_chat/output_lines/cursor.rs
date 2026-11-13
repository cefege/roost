//! Carriage-return and backspace writes for the shared ANSI line decoder.
//! Retains untouched suffix cells and uses the terminal core's Unicode widths.
//! Zero-width scalars attach to the preceding cell, matching its wcwidth mode.

use super::Run;
use rio_unicode::UnicodeWidthChar;

struct Unit {
    run: usize,
    start_byte: usize,
    end_byte: usize,
    start_column: usize,
    end_column: usize,
}

pub(super) fn column(runs: &[Run]) -> usize {
    runs.iter()
        .fold(0, |column, run| end_column(&run.text, column))
}

pub(super) fn prepare(runs: &mut Vec<Run>) {
    let mut index = 0;
    while index < runs.len() {
        let leading = leading_zero_width_bytes(&runs[index].text);
        if leading > 0 {
            let (before, after) = runs.split_at_mut(index);
            if let Some(previous) = before.last_mut() {
                previous.text.push_str(&after[0].text[..leading]);
            }
            drop(after[0].text.drain(..leading));
        }
        if runs[index].text.is_empty() {
            runs.remove(index);
        } else {
            index += 1;
        }
    }
}

pub(super) fn overwrite(runs: &mut Vec<Run>, at: usize, mut inserted: Run) -> usize {
    let leading = leading_zero_width_bytes(&inserted.text);
    if leading > 0 {
        if let Some(previous) = at.checked_sub(1).and_then(|column| locate(runs, column, 0)) {
            runs[previous.run]
                .text
                .insert_str(previous.end_byte, &inserted.text[..leading]);
        }
        drop(inserted.text.drain(..leading));
    }
    if inserted.text.is_empty() {
        return at;
    }
    let end = end_column(&inserted.text, at);
    let mut suffix = take_suffix(runs, at, 0);
    discard_prefix(&mut suffix, end, at);
    runs.push(inserted);
    runs.append(&mut suffix);
    end
}

fn width(character: char, column: usize) -> usize {
    if character == '\t' {
        8 - column % 8
    } else {
        UnicodeWidthChar::width(character).unwrap_or(0)
    }
}

fn end_column(text: &str, start: usize) -> usize {
    text.chars()
        .fold(start, |column, character| column + width(character, column))
}

fn leading_zero_width_bytes(text: &str) -> usize {
    text.char_indices()
        .take_while(|(_, character)| {
            *character != '\t' && UnicodeWidthChar::width(*character) == Some(0)
        })
        .map(|(byte, character)| byte + character.len_utf8())
        .last()
        .unwrap_or(0)
}

fn locate(runs: &[Run], at: usize, start: usize) -> Option<Unit> {
    let mut column = start;
    for (run_index, run) in runs.iter().enumerate() {
        let mut characters = run.text.char_indices().peekable();
        while let Some((byte, character)) = characters.next() {
            let columns = width(character, column);
            let mut end_byte = byte + character.len_utf8();
            while characters.peek().is_some_and(|(_, character)| {
                *character != '\t' && UnicodeWidthChar::width(*character) == Some(0)
            }) {
                if let Some((byte, character)) = characters.next() {
                    end_byte = byte + character.len_utf8();
                }
            }
            if at < column + columns {
                return Some(Unit {
                    run: run_index,
                    start_byte: byte,
                    end_byte,
                    start_column: column,
                    end_column: column + columns,
                });
            }
            column += columns;
        }
    }
    None
}

fn take_suffix(runs: &mut Vec<Run>, at: usize, start: usize) -> Vec<Run> {
    let Some(unit) = locate(runs, at, start) else {
        return Vec::new();
    };
    let mut suffix = runs.split_off(unit.run);
    if let Some(prefix) = split_first(&mut suffix, &unit, at) {
        runs.push(prefix);
    }
    suffix
}

fn discard_prefix(runs: &mut Vec<Run>, at: usize, start: usize) {
    let Some(unit) = locate(runs, at, start) else {
        runs.clear();
        return;
    };
    drop(runs.drain(..unit.run));
    drop(split_first(runs, &unit, at));
}

fn split_first(runs: &mut Vec<Run>, unit: &Unit, at: usize) -> Option<Run> {
    let inside = at > unit.start_column;
    let (before, after, left, right) = if inside {
        (
            unit.start_byte,
            unit.end_byte,
            at - unit.start_column,
            unit.end_column - at,
        )
    } else {
        (unit.start_byte, unit.start_byte, 0, 0)
    };
    if before == 0 && left == 0 && !inside {
        return None;
    }
    let first = &mut runs[0];
    let mut suffix = first.text.split_off(after);
    let mut prefix = std::mem::take(&mut first.text);
    prefix.truncate(before);
    prefix.extend(std::iter::repeat_n(' ', left));
    if right > 0 {
        suffix.insert_str(0, &"        "[..right]);
    }
    first.text = suffix;
    let prefix = (!prefix.is_empty()).then(|| Run {
        text: prefix,
        style: first.style,
        href: first.href.clone(),
    });
    if first.text.is_empty() {
        runs.remove(0);
    }
    prefix
}
