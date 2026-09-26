//! The canonical wire shape of every command kind, built from parts so a
//! frame and the table that names its kind cannot drift apart.

use serde_json::{Value, json};

use super::{DIGEST, EPOCH, SESSION};

/// One frame of every kind, as the wire spells it.
///
/// Built from parts rather than written out, so a frame and the table that
/// names its kind cannot drift apart.
pub fn every_kind() -> Vec<(&'static str, Value)> {
    let capture = [
        ("request_id", json!("r16")),
        (
            "recording_id",
            json!("3f6b2a10-0000-4000-8000-000000000001"),
        ),
        ("capture_id", json!("3f6b2a10-0000-4000-8000-000000000002")),
        ("action", json!("capture")),
        ("reason", json!("manual")),
    ];
    vec![
        frame("attach", &[]),
        frame("cursor-pos", &[("col", json!(3)), ("row", json!(4))]),
        frame("spawn-shell", &[("folder", json!("/srv"))]),
        frame("kill", &[]),
        frame(
            "read-file",
            &[("request_id", json!("r1")), ("path", json!("/etc/hosts"))],
        ),
        frame(
            "read-file-chunk",
            &[
                ("request_id", json!("r2")),
                ("path", json!("/etc/hosts")),
                ("offset", json!(0)),
                ("len", json!(16)),
            ],
        ),
        frame(
            "attachment-probe",
            &[
                ("request_id", json!("r3")),
                ("sha256", json!(DIGEST)),
                ("short_path", json!(false)),
            ],
        ),
        frame(
            "list-dir",
            &[("request_id", json!("r4")), ("path", json!("/srv"))],
        ),
        frame(
            "mkdir",
            &[("request_id", json!("r5")), ("path", json!("/srv/new"))],
        ),
        frame("list-skills", &[("request_id", json!("r6"))]),
        frame("git-diff", &[("request_id", json!("r7"))]),
        frame("set-title", &[("title", json!("build"))]),
        frame("get-home", &[("request_id", json!("r8"))]),
        frame(
            "get-scrollback-cells",
            &[
                ("request_id", json!("r9")),
                ("grid_epoch", json!(EPOCH)),
                ("end_row", json!(100)),
                ("max_rows", json!(50)),
            ],
        ),
        frame(
            "search-scrollback",
            &[
                ("request_id", json!("r10")),
                ("grid_epoch", json!(EPOCH)),
                ("search_id", json!("s-1")),
                ("query", json!("needle")),
                ("case_sensitive", json!(false)),
                ("regex", json!(false)),
                ("max_rows", json!(512)),
                ("max_matches", json!(64)),
            ],
        ),
        frame(
            "cancel-scrollback-search",
            &[
                ("request_id", json!("r11")),
                ("search_request_id", json!("s-1")),
            ],
        ),
        frame(
            "search-scrollback-batch",
            &[
                ("request_id", json!("r12")),
                ("search_id", json!("s-2")),
                ("query", json!("needle")),
                ("case_sensitive", json!(false)),
                (
                    "sessions",
                    json!([{ "session_id": SESSION, "grid_epoch": EPOCH }]),
                ),
                ("max_rows_per_session", json!(512)),
                ("max_matches", json!(64)),
                ("deadline_ms", json!(5_000)),
            ],
        ),
        frame(
            "cancel-scrollback-search-batch",
            &[
                ("request_id", json!("r13")),
                ("search_id", json!("s-2")),
                ("session_ids", json!([SESSION])),
            ],
        ),
        frame("list-attachments", &[("request_id", json!("r14"))]),
        frame(
            "delete-attachment",
            &[
                ("request_id", json!("r15")),
                ("filename", json!("notes.txt")),
            ],
        ),
        frame("diag-terminal-capture", &capture),
        frame("diag-snapshot", &[("request_id", json!(r"r17"))]),
        frame(
            "respawn-if-missing",
            &[("request_id", json!("r18")), ("cwd", json!("/srv"))],
        ),
        frame("detach", &[]),
    ]
}

/// A canonical frame of one kind, with the session every session-scoped
/// command names and the fields the caller adds on top.
/// The kinds whose frame names a session.
///
/// The batch and file frames do not, and they are STRICT: a session id added
/// to one is a key the frame does not define, which is the refusal a caller
/// gets from a build it does not match.
const SESSION_SCOPED: [&str; 14] = [
    "attach",
    "cursor-pos",
    "kill",
    "attachment-probe",
    "git-diff",
    "set-title",
    "get-scrollback-cells",
    "search-scrollback",
    "cancel-scrollback-search",
    "list-attachments",
    "delete-attachment",
    "diag-terminal-capture",
    "respawn-if-missing",
    "detach",
];

pub fn frame(kind: &'static str, extra: &[(&str, Value)]) -> (&'static str, Value) {
    let mut value = serde_json::Map::new();
    value.insert("kind".to_owned(), json!(kind));
    if SESSION_SCOPED.contains(&kind) {
        value.insert("session_id".to_owned(), json!(SESSION));
    }
    for (key, field) in extra {
        value.insert((*key).to_owned(), field.clone());
    }
    (kind, Value::Object(value))
}
