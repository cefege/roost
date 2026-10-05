//! The browser halves of the terminal chrome: blurring a mounted field, and
//! the file input a picker opens. Target-independent rules live in the
//! components; this module exists so a `web_sys` type never appears in one.
//! Ports the `HTMLTextAreaElement` / `HTMLInputElement` reaches of
//! `apps/web/src/components/terminal/TerminalComposeButton.tsx` and
//! `apps/web/src/renderer/terminalComposeSelection.ts`.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// Drop the focus an element holds, so the terminal's own shortcuts resume
/// routing. A refused element is a field that already lost focus.
#[cfg(target_arch = "wasm32")]
pub fn blur(element: &MountedData) {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let Some(element) = element.try_as_web_event() else {
        return;
    };
    if let Some(field) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
        let _ = field.blur();
    }
}

/// Nothing to blur outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn blur(_element: &MountedData) {}

/// Size a field to its content, the way a controlled textarea has to be sized
/// after every write. The `auto` write first is what makes this a MEASUREMENT
/// and not a copy of the last height: with a previous pixel height in place the
/// element reports that height back as `scrollHeight` and the field can only
/// ever grow to where it already is. The cap stays in CSS.
///
/// The `auto` write collapses the field to one row for the length of the
/// measurement, which clamps its scroll offset to zero; a capped field would
/// then come back scrolled to its first line with the caret out of view. The
/// offset is put back once the height is.
#[cfg(target_arch = "wasm32")]
pub fn auto_grow(field: &MountedData) {
    let Some(area) = text_area(field) else {
        return;
    };
    let scrolled = area.scroll_top();
    let style = area.style();
    if style.set_property("height", "auto").is_err() {
        return;
    }
    let content = area.scroll_height();
    let _ = style.set_property("height", &format!("{content}px"));
    area.set_scroll_top(scrolled);
}

/// Nothing to size outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn auto_grow(_field: &MountedData) {}

/// Keep a capped field's caret line in view, and the ghost mirror painted
/// over it in step. A programmatic write — a restored draft, a dictated tail —
/// leaves the caret at the end without scrolling the element the way a
/// keystroke does, so a caret at the end pins the field to its last line. A
/// caret anywhere else was put there by the reader, who scrolled to it, and
/// that scroll is left alone: pinning it to the end would yank an edit in
/// the middle of a long draft out of view. The mirror is reached through the
/// field's shared parent rather than carried separately, so the two boxes
/// cannot come to be scrolled independently of each other.
#[cfg(target_arch = "wasm32")]
pub fn keep_caret_visible(field: &MountedData) {
    let Some(area) = text_area(field) else {
        return;
    };
    let length = area.value().encode_utf16().count();
    let caret = area.selection_end().ok().flatten().map(|end| end as usize);
    if caret.is_none_or(|end| end >= length) {
        area.set_scroll_top(area.scroll_height());
    }
    let offset = area.scroll_top();
    let mirror = area
        .parent_element()
        .and_then(|parent| parent.query_selector(".term-chat__ghost").ok().flatten());
    if let Some(mirror) = mirror
        && let Some(mirror) = mirror.dyn_ref::<web_sys::HtmlElement>()
    {
        mirror.set_scroll_top(offset);
    }
}

/// Nothing to scroll outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn keep_caret_visible(_field: &MountedData) {}

/// The textarea behind a mounted field, or `None` for any other element.
#[cfg(target_arch = "wasm32")]
fn text_area(field: &MountedData) -> Option<web_sys::HtmlTextAreaElement> {
    use dioxus::web::WebEventExt as _;

    let element: web_sys::Element = field.try_as_web_event()?;
    element.dyn_into::<web_sys::HtmlTextAreaElement>().ok()
}

/// Open a file input, from inside the gesture that asked for it.
///
/// iOS refuses a programmatic click that is not inside a user gesture, which
/// is why this sits beside the composer's attach button rather than in a
/// deferred "click when ready" the composer schedules.
#[cfg(target_arch = "wasm32")]
pub fn open_file_chooser(input: &MountedData) {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let Some(element) = input.try_as_web_event() else {
        return;
    };
    if let Some(field) = element.dyn_ref::<web_sys::HtmlInputElement>() {
        field.click();
    }
}

/// Nothing to open outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_file_chooser(_input: &MountedData) {}

/// The files a `change` event carries. The event is the authority, not a
/// re-read of the input: a cancelled picker fires nothing, and an event whose
/// files were cleared reads as empty rather than as the previous selection.
#[cfg(target_arch = "wasm32")]
pub fn files_of_event(event: &FormEvent) -> Vec<web_sys::File> {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let Some(input) = event
        .try_as_web_event()
        .and_then(|web_event| web_event.target())
        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return Vec::new();
    };
    let Some(list) = input.files() else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index))
        .collect()
}

/// No change event outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn files_of_event(_event: &FormEvent) -> Vec<web_sys::File> {
    Vec::new()
}

/// A file's name, as the composer and the transfer card print it.
#[cfg(target_arch = "wasm32")]
pub fn file_name(file: &web_sys::File) -> String {
    file.name()
}

/// No file outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn file_name(_file: &web_sys::File) -> String {
    String::new()
}

/// A file's byte length, or zero when the browser refuses to say.
#[cfg(target_arch = "wasm32")]
pub fn file_size(file: &web_sys::File) -> u64 {
    file.size().max(0.0) as u64
}

/// No file outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub fn file_size(_file: &web_sys::File) -> u64 {
    0
}

/// A blob URL for a chosen file, which is what the transfer card's local
/// preview paints while the dedup probe is still in flight. The caller owns the
/// URL and revokes it with [`revoke_preview`].
#[cfg(target_arch = "wasm32")]
pub async fn preview_url(file: &web_sys::File) -> Option<String> {
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(file.type_().as_str());
    let blob =
        web_sys::Blob::new_with_blob_sequence_and_options(&js_sys::Array::of1(file), &options)
            .ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok()
}

/// No preview outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub async fn preview_url(_file: &web_sys::File) -> Option<String> {
    None
}

/// Release a preview the transfer card no longer shows.
pub fn revoke_preview(url: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Err(error) = web_sys::Url::revoke_object_url(url) {
            tracing::debug!(target: "terminal", ?error, "preview URL was already released");
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = url;
    }
}

/// Read a `File` into memory, for the chunker the direct carrier feeds.
///
/// `Blob.arrayBuffer()` resolves with the bytes themselves. A `FileReader`'s
/// `load` callback resolves with its ProgressEvent, which is not a buffer.
#[cfg(target_arch = "wasm32")]
pub async fn read_bytes(file: &web_sys::File) -> Option<Vec<u8>> {
    use wasm_bindgen_futures::JsFuture;

    let buffer = JsFuture::from(file.array_buffer()).await.ok()?;
    buffer
        .dyn_into::<js_sys::ArrayBuffer>()
        .ok()
        .map(|array| js_sys::Uint8Array::new(&array).to_vec())
}

/// No file outside a browser.
#[cfg(not(target_arch = "wasm32"))]
pub async fn read_bytes(_file: &web_sys::File) -> Option<Vec<u8>> {
    None
}
