//! The small shared subset of Server-Sent Events used by provider clients.
//! It accepts split CRLF/LF input lines, ignores comments and joins multiple
//! `data` fields as required by the SSE format.

pub(super) fn consume_line(line: &str, data: &mut Vec<String>, completed: &mut Vec<String>) {
    if line.is_empty() {
        if !data.is_empty() {
            completed.push(data.join("\n"));
            data.clear();
        }
    } else if let Some(value) = line.strip_prefix("data:") {
        data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
    }
}
