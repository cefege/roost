//! `roost api cells` and `attach`: the two verbs that read a session's
//! scrollback and put a file into it. Called by `api::mod`; depends on the
//! generated `SessionsGetScrollbackCells` and `AttachFileChunk` methods and on
//! `api::client`.
//!
//! WHY THE RANGE GOES TO STDERR. `cells` prints row text and nothing else, so
//! it can be diffed against a previous run or piped into a pager. The range it
//! served is context rather than output — but it is also what makes a short
//! read explicable, so it is printed rather than dropped.
//!
//! WHY THE UPLOAD IS SERIAL. The worker refuses an out-of-order `seq`, so a
//! parallel upload is a rejected one. Serial, in the order the operator named
//! the files, is the only shape the contract accepts.

use std::path::Path;
use std::process::ExitCode;

use roost_proto::{AttachFileChunkRequest, SessionsGetScrollbackCellsRequest};
use sha2::{Digest, Sha256};

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// How many rows `cells` serves when the operator names no count.
pub const DEFAULT_ROWS: u32 = 40;

/// One upload's worth of bytes. Four megabytes clears the coordinator's own
/// 8 MiB body ceiling with room for the envelope.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Scrollback rows, oldest first, as plain text.
pub async fn cells(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let max_rows = args.count("--rows", DEFAULT_ROWS)?;
    // `end_row` is exclusive, and a headless caller holds no viewport epoch, so
    // the empty epoch binds the request to whatever the worker is serving now.
    // A value past any possible scrollback is how "the newest rows" is asked
    // for; the worker clamps it to the rows it actually retains.
    let end_row = match args.optional_value("--end") {
        None => u64::MAX,
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            CommandFailure::usage(format!(
                "roost api cells: --end must be a row number, got {raw:?}"
            ))
        })?,
    };
    let response = api
        .answer(
            api.stub()
                .sessions_get_scrollback_cells(SessionsGetScrollbackCellsRequest {
                    session_id: session.to_string(),
                    end_row,
                    max_rows,
                    grid_epoch: String::new(),
                    ..Default::default()
                }),
        )
        .await?;
    out.progress(&format!(
        "rows {}..{} of {} (cols {})",
        response.start_row, response.end_row, response.scrollback_total, response.cols
    ));
    for row in &response.rows {
        let text: String = row.spans.iter().map(|span| span.text.as_str()).collect();
        out.answer(text.trim_end());
    }
    Ok(ExitCode::SUCCESS)
}

/// Upload one or more local files into a session, printing each absolute path
/// the worker created — one per line, and nothing else.
pub async fn attach(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?.to_string();
    let short_path = args.has("--short-path");
    for path in &args.positionals[1..] {
        let absolute = upload(api, &session, Path::new(path), short_path, out).await?;
        out.answer(&absolute);
    }
    Ok(ExitCode::SUCCESS)
}

async fn upload(
    api: &CoordinatorApi,
    session: &str,
    path: &Path,
    short_path: bool,
    out: &mut dyn ApiOutput,
) -> Result<String, CommandFailure> {
    let bytes = std::fs::read(path).map_err(|error| {
        CommandFailure::usage(format!(
            "roost api attach: cannot read {}: {error}",
            path.display()
        ))
    })?;
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".to_string());
    let upload_id = upload_identity(session, &filename, bytes.len());
    let mut absolute = String::new();
    let mut sequence: u32 = 0;
    // At least one chunk runs even for an empty file, so a zero-byte upload
    // still creates the file and still comes back with a path.
    let mut offset = 0;
    loop {
        let end = (offset + CHUNK_BYTES).min(bytes.len());
        let last = end == bytes.len();
        let response = api
            .answer(api.stub().attach_file_chunk(AttachFileChunkRequest {
                upload_id: upload_id.clone(),
                session_id: session.to_string(),
                filename: filename.clone(),
                short_path,
                data: bytes[offset..end].to_vec(),
                last,
                seq: sequence,
                ..Default::default()
            }))
            .await?;
        sequence += 1;
        if last {
            absolute = response.abs_path;
            break;
        }
        out.progress(&format!("{filename}: {end}/{} bytes", bytes.len()));
        offset = end;
    }
    Ok(absolute)
}

/// An upload's identity, derived from what the upload is rather than from a
/// counter.
///
/// A counter would have to live somewhere both processes agree on, and there
/// is no such place here: the worker is the only thing assembling these chunks.
/// Deriving it from the session, the name and the size means two concurrent
/// uploads of two different files cannot collide, and a re-run of the same
/// upload resumes against the same identity rather than orphaning the first.
fn upload_identity(session: &str, filename: &str, bytes: usize) -> String {
    let mut digest = Sha256::new();
    digest.update(session.as_bytes());
    digest.update(b"\0");
    digest.update(filename.as_bytes());
    digest.update(b"\0");
    digest.update(bytes.to_string().as_bytes());
    format!("cli-{}", hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::upload_identity;

    #[test]
    fn two_uploads_of_different_files_never_share_an_identity() {
        assert_ne!(
            upload_identity("session-1", "a.txt", 10),
            upload_identity("session-1", "b.txt", 10)
        );
    }

    #[test]
    fn the_same_upload_names_the_same_identity_so_a_rerun_resumes() {
        assert_eq!(
            upload_identity("session-1", "a.txt", 10),
            upload_identity("session-1", "a.txt", 10)
        );
    }

    #[test]
    fn two_sessions_uploading_the_same_name_do_not_collide() {
        assert_ne!(
            upload_identity("session-1", "a.txt", 10),
            upload_identity("session-2", "a.txt", 10)
        );
    }
}
