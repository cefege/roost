//! Where a completed attachment lands. Called by the composer's insertion step
//! once an upload returned its worker-side path; decides whether that path may
//! be typed into a PTY at all. Ported from `attachmentInsertion.ts`. Depends on
//! nothing but the worker's `os` string, the same one the rest of the client
//! reads, and on the shared POSIX quoter's algorithm.

/// The quoted path, or `None` when it must not reach a PTY.
///
/// `worker_os` is the worker's `os` column (`linux`, `darwin`, `win32`), not
/// the reader's platform: the path is typed into the WORKER's shell, so the
/// worker's rules are the ones that decide whether quoting means anything.
#[must_use]
pub fn safe_attachment_insertion(worker_os: &str, abs_path: &str) -> Option<String> {
    if contains_control_character(abs_path) {
        return None;
    }
    match worker_os {
        "linux" | "darwin" => Some(posix_quote(abs_path)),
        _ => None,
    }
}

/// Every C0 control character, DEL, and every C1 control character.
///
/// A path carrying one of these is not a path a worker could have written on
/// disk, and quoting does not neutralise them: a newline or an escape inside
/// single quotes is still read by the line discipline before the shell parses
/// anything.
fn contains_control_character(value: &str) -> bool {
    value.chars().any(|character| {
        let code = u32::from(character);
        code <= 0x1f || (0x7f..=0x9f).contains(&code)
    })
}

/// Wrap `value` in single quotes, escaping an embedded quote by closing,
/// escaping and reopening.
///
/// A local copy of `roost_platform::posix_shell_quote`, which owns this for the
/// whole product. It is duplicated rather than shared only because this crate
/// has no dependency on the platform crate; the day it takes one, this
/// function is deleted and the call below names the owner directly.
fn posix_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}
