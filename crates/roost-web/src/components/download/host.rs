//! The browser half of a worker-to-browser download: the `FilesReadChunk` loop
//! over the coordinator, and the Blob save that ends it. Called by
//! `DownloadButton`; the run's checks and card rules are
//! `roost_client_core::client::download`'s.

use crate::pump::Pump;

/// Download one worker file into the browser's downloads. The transfer card is
/// the run's whole report; dismissing it cancels the run.
#[cfg(target_arch = "wasm32")]
pub fn start_download(pump: &Pump, worker_fp: &str, path: &str) {
    wasm_bindgen_futures::spawn_local(run_download(
        pump.clone(),
        worker_fp.to_owned(),
        path.to_owned(),
    ));
}

/// No download outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_download(_pump: &Pump, _worker_fp: &str, _path: &str) {}

/// The name the browser saves the file under: the path's last segment.
#[must_use]
pub fn download_file_name(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|segment| !segment.is_empty())
}

#[cfg(target_arch = "wasm32")]
async fn run_download(pump: Pump, worker_fp: String, path: String) {
    use roost_client_core::client::download::{
        DOWNLOAD_CHUNK_BYTES, DownloadError, DownloadProgress, DownloadRun,
    };
    use roost_client_core::client::rpc::calls::files::ReadFileChunk;

    use crate::components::terminal_chrome::upload_id::{mint_upload_id, now_ms};
    use crate::platform::file_save::save_file_bytes;

    let Some(file_name) = download_file_name(&path).map(str::to_owned) else {
        tracing::warn!(target: "download", %path, "a path with no file name cannot be downloaded");
        return;
    };
    let Some(download_id) = mint_upload_id() else {
        tracing::warn!(target: "download", %path, "no download id could be minted");
        return;
    };
    let mut run =
        pump.write_store(|store| DownloadRun::begin(store, &download_id, &file_name, now_ms()));
    tracing::info!(target: "download", worker = %worker_fp, %path, route = "coordinator", "download started");
    let mut offset = 0_u64;
    let outcome: Result<Vec<u8>, DownloadError> = loop {
        let request = ReadFileChunk {
            worker_fp: worker_fp.clone(),
            path: path.clone(),
            offset,
            len: DOWNLOAD_CHUNK_BYTES,
        };
        let chunk = match pump.rpc().call(&request).await {
            Ok(chunk) => chunk,
            Err(error) => {
                tracing::warn!(target: "download", %path, %error, "download chunk failed");
                pump.write_store(|store| run.fail(store, &error.to_string(), now_ms()));
                return;
            }
        };
        match pump.write_store(|store| run.accept_chunk(store, chunk, now_ms())) {
            Ok(DownloadProgress::More { offset: next }) => offset = next,
            Ok(DownloadProgress::Complete(bytes)) => break Ok(bytes),
            Err(error) => break Err(error),
        }
    };
    match outcome {
        Ok(bytes) => {
            let saved = save_file_bytes(&file_name, "application/octet-stream", &bytes);
            pump.write_store(|store| run.finish(store, saved, now_ms()));
            tracing::info!(target: "download", %path, bytes = bytes.len(), saved, "download settled");
        }
        Err(DownloadError::Cancelled) => {
            tracing::info!(target: "download", %path, "download cancelled");
        }
        Err(error) => {
            tracing::warn!(target: "download", %path, %error, "download refused");
            pump.write_store(|store| run.fail(store, &error.to_string(), now_ms()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::download_file_name;

    #[test]
    fn the_saved_name_is_the_last_path_segment() {
        assert_eq!(
            download_file_name("/home/me/report.pdf"),
            Some("report.pdf")
        );
        assert_eq!(download_file_name("C:\\logs\\a.txt"), Some("a.txt"));
        assert_eq!(download_file_name("/"), None);
    }
}
