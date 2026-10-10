//! Ported from oh-my-pi crates/pi-edit/src/modes/hashline/apply.rs and clipboard.rs (MIT).
//! Applies line operations against immutable snapshot coordinates and resolves registers.
//! Descending splice order preserves every authored original line number.

use super::{EditStore, Operation, normalize_text};

struct Splice {
    start: usize,
    end: usize,
    sequence: usize,
    replacement: Vec<String>,
}

pub(super) fn apply_operations(
    text: &str,
    operations: &[Operation],
    store: &mut EditStore,
) -> Result<String, String> {
    if operations
        .iter()
        .any(|operation| matches!(operation, Operation::Remove))
    {
        if operations.len() != 1 {
            return Err("REM must be the only operation in a file section.".into());
        }
        return Ok(String::new());
    }
    let normalized = normalize_text(text);
    let terminal_newline = normalized.ends_with('\n');
    let mut lines = normalized
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if terminal_newline {
        lines.pop();
    }
    let mut splices = Vec::new();
    for (sequence, operation) in operations.iter().enumerate() {
        match operation {
            Operation::Replace {
                start,
                end,
                lines: replacement,
            } => {
                let (start, end) = checked_range(*start, *end, lines.len())?;
                splices.push(Splice {
                    start,
                    end,
                    sequence,
                    replacement: replacement.clone(),
                });
            }
            Operation::Cut {
                start,
                end,
                register,
            } => {
                let (start_idx, end_idx) = checked_range(*start, *end, lines.len())?;
                store.set_register(register.as_deref(), lines[start_idx..end_idx].to_vec());
                splices.push(Splice {
                    start: start_idx,
                    end: end_idx,
                    sequence,
                    replacement: Vec::new(),
                });
            }
            Operation::InsertBefore { line, lines: body } => {
                let index = line
                    .checked_sub(1)
                    .and_then(|number| usize::try_from(number).ok())
                    .filter(|number| *number < lines.len() || (*line == 1 && lines.is_empty()))
                    .ok_or_else(|| out_of_range(*line, lines.len()))?;
                splices.push(Splice {
                    start: index,
                    end: index,
                    sequence,
                    replacement: body.clone(),
                });
            }
            Operation::InsertAfter { line, lines: body } => {
                let index = usize::try_from(*line)
                    .ok()
                    .filter(|number| *number <= lines.len())
                    .ok_or_else(|| out_of_range(*line, lines.len()))?;
                splices.push(Splice {
                    start: index,
                    end: index,
                    sequence,
                    replacement: body.clone(),
                });
            }
            Operation::InsertEnd { lines: body } => splices.push(Splice {
                start: lines.len(),
                end: lines.len(),
                sequence,
                replacement: body.clone(),
            }),
            Operation::PasteBefore { line, register } => {
                let index = line
                    .checked_sub(1)
                    .and_then(|number| usize::try_from(number).ok())
                    .filter(|number| *number < lines.len() || (*line == 1 && lines.is_empty()))
                    .ok_or_else(|| out_of_range(*line, lines.len()))?;
                let copied = pasted_lines(store, register.as_deref(), false)?;
                splices.push(Splice {
                    start: index,
                    end: index,
                    sequence,
                    replacement: copied,
                });
            }
            Operation::PasteAfter { line, register } => {
                let index = usize::try_from(*line)
                    .ok()
                    .filter(|number| *number <= lines.len())
                    .ok_or_else(|| out_of_range(*line, lines.len()))?;
                let copied = pasted_lines(store, register.as_deref(), false)?;
                splices.push(Splice {
                    start: index,
                    end: index,
                    sequence,
                    replacement: copied,
                });
            }
            Operation::PasteEnd { register } => {
                let copied = pasted_lines(store, register.as_deref(), false)?;
                splices.push(Splice {
                    start: lines.len(),
                    end: lines.len(),
                    sequence,
                    replacement: copied,
                });
            }
            Operation::PasteRange {
                start,
                end,
                register,
            } => {
                let (start_idx, end_idx) = checked_range(*start, *end, lines.len())?;
                let copied = pasted_lines(store, register.as_deref(), true)?;
                splices.push(Splice {
                    start: start_idx,
                    end: end_idx,
                    sequence,
                    replacement: copied,
                });
            }
            Operation::Remove | Operation::Move(_) => {}
        }
    }
    splices.sort_by_key(|splice| {
        (
            std::cmp::Reverse(splice.start),
            std::cmp::Reverse(splice.sequence),
        )
    });
    for splice in splices {
        lines.splice(splice.start..splice.end, splice.replacement);
    }
    let mut result = lines.join("\n");
    if terminal_newline && !result.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

fn pasted_lines(
    store: &EditStore,
    register: Option<&str>,
    replaces_range: bool,
) -> Result<Vec<String>, String> {
    if let Some(lines) = store.register(register) {
        return Ok(lines.to_vec());
    }
    if let Some(name) = register {
        if replaces_range {
            return Err(format!(
                "`@{name}` is empty — no `CUT … @{name}` precedes this op in this call and no persisted register has that name — so pasting it over a range would delete those lines and write nothing back. Capture the register first (`CUT … @{name}`), or use `CUT` if deleting the range is what you meant."
            ));
        }
        return Ok(Vec::new());
    }
    Err("Nothing to paste: no unlabeled `CUT` precedes this `PUT` in this call, and the anonymous register never carries across calls. Put `CUT N.=M` / `CUT N*` above it, or use named registers (`CUT … @name` → `PUT … @name`) for cross-call moves.".into())
}

fn checked_range(start: u32, end: u32, line_count: usize) -> Result<(usize, usize), String> {
    let start_idx = start
        .checked_sub(1)
        .and_then(|line| usize::try_from(line).ok())
        .ok_or_else(|| "Line number must be >= 1.".to_owned())?;
    let end_idx = usize::try_from(end).map_err(|_| "Line number is too large.".to_owned())?;
    if end_idx > line_count {
        return Err(out_of_range(end, line_count));
    }
    Ok((start_idx, end_idx))
}

fn out_of_range(line: u32, line_count: usize) -> String {
    format!("Line {line} does not exist (file has {line_count} lines)")
}
