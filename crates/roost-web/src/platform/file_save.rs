//! Saving a generated text file through the browser's download path: a blob
//! URL on a detached `<a download>` that is clicked once and released. The
//! sidebar's Remote Desktop action saves its `.rdp` file here
//! (`machine_actions::MachineLaunch::Download`).

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

/// How long the blob URL outlives the click: the download reads it after the
/// click returns, so revoking it synchronously would cancel the save.
#[cfg(target_arch = "wasm32")]
const BLOB_URL_RELEASE_MS: i32 = 1_000;

/// Save `contents` as `file_name`, answering whether the browser took it.
#[cfg(target_arch = "wasm32")]
pub fn save_text_file(file_name: &str, mime_type: &str, contents: &str) -> bool {
    let saved = start_save(file_name, mime_type, contents);
    match &saved {
        Ok(()) => tracing::info!(target: "download", file_name, "a generated file was saved"),
        Err(error) => {
            tracing::warn!(target: "download", file_name, ?error, "the browser refused the save");
        }
    }
    saved.is_ok()
}

#[cfg(target_arch = "wasm32")]
fn start_save(
    file_name: &str,
    mime_type: &str,
    contents: &str,
) -> Result<(), wasm_bindgen::JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime_type);
    let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(contents));
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let anchor: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    anchor.click();
    let release = wasm_bindgen::closure::Closure::once_into_js(move || {
        if let Err(error) = web_sys::Url::revoke_object_url(&url) {
            tracing::debug!(target: "download", ?error, "blob URL was already released");
        }
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(
        release.unchecked_ref(),
        BLOB_URL_RELEASE_MS,
    )?;
    Ok(())
}

/// A native build has no download path.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_text_file(_file_name: &str, _mime_type: &str, _contents: &str) -> bool {
    false
}
