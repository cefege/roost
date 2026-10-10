//! Ported from oh-my-pi crates/pi-edit/src/modes/hashline/tokenizer.rs and parser.rs (MIT).
//! Converts hashline headers and operation rows into immutable file sections.
//! Tree-sitter block operations are rejected because their resolver is not ported.

use super::model::{Operation, PatchSection};

const MAX_EXPANDED_RANGE_LINES: u32 = 100_000;
const CUT_TAKES_NO_BODY: &str = "`CUT` deletes (and captures) the named lines and takes no \
    body. To write new content, use `PUT N.=M:` with `+TEXT` rows.";
const COLONLESS_SPAN_PUT: &str = "Colonless `PUT` is clipboard-backed, and span targets need a \
    named register (`PUT 5.=9 @name`); the anonymous register pastes only at gaps (`PUT >40`). \
    To write literal content, add `:` and `+TEXT` rows.";
const COLON_ON_REGISTER_PUT: &str = "`PUT … @name` pastes the register and never takes `:` — \
    the colon promises body rows. Drop the colon (`PUT >40 @name`), or drop `@name` and write \
    `+TEXT` body rows.";
const EMPTY_INSERT: &str = "`PUT <N:` / `PUT >N:` promises body rows and got none. Write \
    `+TEXT` rows, or drop the `:` to paste a register (`PUT >N` = anonymous, `PUT >N @name` = \
    named).";
const REM_TAKES_NO_BODY: &str = "`REM` deletes the whole file and takes no body or line ops. \
    Issue it alone under the header.";
const MOVE_TAKES_NO_BODY: &str = "`MV DEST` does not take body rows. Put line edits above the \
    `MV` row; the destination path follows `MV` on the same line.";

/// Parse a complete hashline payload containing one or more file sections.
pub fn parse_sections(input: &str) -> Result<Vec<PatchSection>, String> {
    let mut sections = Vec::new();
    let mut current: Option<PatchSection> = None;
    let mut rows = input.lines().enumerate().peekable();
    while let Some((idx, raw)) = rows.next() {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        if text.starts_with('[') && text.ends_with(']') {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let inner = &text[1..text.len() - 1];
            let (path, tag) = inner
                .rsplit_once('#')
                .ok_or_else(|| format!("line {}: missing hashline snapshot tag.", idx + 1))?;
            if path.is_empty()
                || tag.len() != 4
                || !tag.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!(
                    "line {}: invalid hashline file header; expected [PATH#TAG] with a 4-hex tag.",
                    idx + 1
                ));
            }
            current = Some(PatchSection {
                path: path.to_owned(),
                tag: tag.to_ascii_uppercase(),
                operations: Vec::new(),
            });
            continue;
        }
        let Some(section) = current.as_mut() else {
            return Err(format!(
                "line {}: expected a [PATH#TAG] file header.",
                idx + 1
            ));
        };
        if (text.starts_with("PUT ") || text.starts_with("CUT "))
            && text
                .strip_suffix(':')
                .unwrap_or(text)
                .split_whitespace()
                .nth(1)
                .is_some_and(|locator| locator.contains('*'))
        {
            return Err(format!(
                "line {}: tree-sitter block operations (`N*` and `>N*`) are not supported.",
                idx + 1
            ));
        }
        if text == "REM" {
            if !section.operations.is_empty() {
                return Err(REM_TAKES_NO_BODY.into());
            }
            if rows.peek().is_some_and(|(_, next)| next.starts_with('+')) {
                return Err(REM_TAKES_NO_BODY.into());
            }
            section.operations.push(Operation::Remove);
            continue;
        }
        if let Some(destination) = text.strip_prefix("MV ") {
            if section
                .operations
                .iter()
                .any(|operation| matches!(operation, Operation::Remove | Operation::Move(_)))
            {
                return Err(format!(
                    "line {}: only one file-level op (`REM` or `MV`) per section. Merge them under one header.",
                    idx + 1
                ));
            }
            if rows.peek().is_some_and(|(_, next)| next.starts_with('+')) {
                return Err(MOVE_TAKES_NO_BODY.into());
            }
            let Some(destination) = unquote(destination.trim()) else {
                return Err(format!(
                    "line {}: MV has an unmatched quoted destination.",
                    idx + 1
                ));
            };
            if destination.is_empty() {
                return Err(format!("line {}: MV requires a destination path.", idx + 1));
            }
            section
                .operations
                .push(Operation::Move(destination.to_owned()));
            continue;
        }
        let (header, has_body) = match text.strip_suffix(':') {
            Some(header) => (header.trim(), true),
            None => (text, false),
        };
        if text.contains(':') && !has_body {
            return Err(format!(
                "line {}: malformed operation header `{text}`.",
                idx + 1
            ));
        }
        let mut body = Vec::new();
        if has_body {
            while let Some((_, next)) = rows.peek().copied() {
                let Some(payload) = next.strip_prefix('+') else {
                    break;
                };
                body.push(payload.to_owned());
                rows.next();
            }
        }
        parse_operation(header, has_body, body, idx + 1, &mut section.operations)?;
    }
    if let Some(section) = current {
        sections.push(section);
    }
    if sections.is_empty() {
        return Err("Missing hashline file header.".into());
    }
    Ok(sections)
}

fn parse_operation(
    header: &str,
    has_body: bool,
    body: Vec<String>,
    line: usize,
    operations: &mut Vec<Operation>,
) -> Result<(), String> {
    if operations
        .iter()
        .any(|operation| matches!(operation, Operation::Remove))
    {
        return Err("`REM` deletes the whole file and cannot be combined with line ops.".into());
    }
    if let Some(range) = header.strip_prefix("CUT ") {
        let (locator, register) = split_register(range, line)?;
        let (start, end) = parse_range(locator)
            .ok_or_else(|| format!("line {line}: invalid CUT range `{locator}`."))?;
        if has_body {
            return Err(format!("line {line}: {CUT_TAKES_NO_BODY}"));
        }
        operations.push(Operation::Cut {
            start,
            end,
            register,
        });
        return Ok(());
    }
    let Some(locator) = header.strip_prefix("PUT ") else {
        return Err(format!("line {line}: unrecognized operation `{header}`."));
    };
    let (target, register) = split_register(locator, line)?;
    if has_body && register.is_some() {
        return Err(format!("line {line}: {COLON_ON_REGISTER_PUT}"));
    }
    if let Some((start, end)) = parse_range(target) {
        match (has_body, register) {
            (true, None) => operations.push(Operation::Replace {
                start,
                end,
                lines: body,
            }),
            (false, Some(register)) => operations.push(Operation::PasteRange {
                start,
                end,
                register: Some(register),
            }),
            (false, None) => return Err(format!("line {line}: {COLONLESS_SPAN_PUT}")),
            (true, Some(_)) => return Err(format!("line {line}: {COLON_ON_REGISTER_PUT}")),
        }
    } else if has_body && body.is_empty() {
        return Err(format!("line {line}: {EMPTY_INSERT}"));
    } else if target == ">$" {
        push_gap(
            Operation::InsertEnd { lines: body },
            Operation::PasteEnd { register },
            has_body,
            line,
            operations,
        )?;
    } else if let Some(anchor) = target.strip_prefix('<').and_then(parse_positive_line) {
        push_gap(
            Operation::InsertBefore {
                line: anchor,
                lines: body,
            },
            Operation::PasteBefore {
                line: anchor,
                register,
            },
            has_body,
            line,
            operations,
        )?;
    } else if let Some(anchor) = target.strip_prefix('>').and_then(parse_positive_line) {
        push_gap(
            Operation::InsertAfter {
                line: anchor,
                lines: body,
            },
            Operation::PasteAfter {
                line: anchor,
                register,
            },
            has_body,
            line,
            operations,
        )?;
    } else {
        return Err(format!("line {line}: invalid PUT locator `{target}`."));
    }
    Ok(())
}

fn push_gap(
    body_operation: Operation,
    paste_operation: Operation,
    has_body: bool,
    line: usize,
    operations: &mut Vec<Operation>,
) -> Result<(), String> {
    match (has_body, paste_operation) {
        (true, _) => operations.push(body_operation),
        (
            false,
            Operation::PasteBefore {
                register: Some(register),
                line: anchor,
            },
        ) => operations.push(Operation::PasteBefore {
            line: anchor,
            register: Some(register),
        }),
        (
            false,
            Operation::PasteAfter {
                register: Some(register),
                line: anchor,
            },
        ) => operations.push(Operation::PasteAfter {
            line: anchor,
            register: Some(register),
        }),
        (
            false,
            Operation::PasteBefore {
                register: None,
                line: anchor,
            },
        ) => operations.push(Operation::PasteBefore {
            line: anchor,
            register: None,
        }),
        (
            false,
            Operation::PasteAfter {
                register: None,
                line: anchor,
            },
        ) => operations.push(Operation::PasteAfter {
            line: anchor,
            register: None,
        }),
        (false, Operation::PasteEnd { register: None }) => {
            operations.push(Operation::PasteEnd { register: None })
        }
        (
            false,
            Operation::PasteEnd {
                register: Some(register),
            },
        ) => operations.push(Operation::PasteEnd {
            register: Some(register),
        }),
        _ => return Err(format!("line {line}: gap PUT requires a body or register.")),
    }
    Ok(())
}

fn split_register(value: &str, line: usize) -> Result<(&str, Option<String>), String> {
    let mut fields = value.split_whitespace();
    let target = fields.next().unwrap_or_default();
    let register = fields.next();
    if fields.next().is_some() {
        return Err(format!(
            "line {line}: expected one locator and an optional register."
        ));
    }
    let Some(register) = register else {
        return Ok((target, None));
    };
    let Some(name) = register.strip_prefix('@') else {
        return Err(format!("line {line}: invalid register `{register}`."));
    };
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("line {line}: invalid register `{register}`."));
    }
    Ok((target, Some(name.to_owned())))
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    let (start, end) = value.split_once(".=")?;
    let start = parse_positive_line(start.trim())?;
    let end = parse_positive_line(end.trim())?;
    let span = end.checked_sub(start)?.checked_add(1)?;
    (span <= MAX_EXPANDED_RANGE_LINES).then_some((start, end))
}

fn parse_positive_line(value: &str) -> Option<u32> {
    let number = value.parse::<u32>().ok()?;
    (number > 0).then_some(number)
}

fn unquote(path: &str) -> Option<&str> {
    for quote in ['"', '\''] {
        if let Some(value) = path
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
        {
            return Some(value);
        }
    }
    if path.starts_with(['"', '\'']) {
        None
    } else {
        Some(path)
    }
}
