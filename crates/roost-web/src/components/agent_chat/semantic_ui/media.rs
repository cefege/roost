//! Authenticated previews for OMP `media` nodes.
//! Paths resolve against the owning conversation's worker and cwd, and every
//! preview URL remains owned by this component until path change or unmount.
use std::cell::Cell;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use super::media_view::MediaImageView;
#[cfg(target_arch = "wasm32")]
use crate::components::terminal_chrome::dom::preview_bytes;
#[cfg(target_arch = "wasm32")]
use crate::components::terminal_chrome::dom::revoke_preview;
#[cfg(target_arch = "wasm32")]
use crate::platform::worker_paths::resolve_worker_path;
#[cfg(target_arch = "wasm32")]
use crate::pump::{Pump, use_store};
use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::browse::ReadFileRevision;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::root::captured_generation_is_current;

#[derive(Clone, PartialEq)]
pub(super) struct MediaContext {
    pub worker_fp: String,
    pub cwd: String,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MediaViewTransform {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}
#[cfg(target_arch = "wasm32")]
impl Default for MediaViewTransform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Default)]
pub(super) struct MediaLinkContext(pub BTreeMap<(String, String), MediaViewTransform>);

#[derive(Clone, PartialEq)]
enum Preview {
    Idle,
    #[cfg(target_arch = "wasm32")]
    Loading,
    #[cfg(target_arch = "wasm32")]
    Ready {
        url: String,
        mime: String,
    },
    #[cfg(target_arch = "wasm32")]
    Failed(String),
}

#[component]
pub(super) fn MediaFilePreview(
    path: String,
    alt: String,
    revision: Option<String>,
    link: Option<String>,
    surface: String,
) -> Element {
    let media_context = use_context::<Signal<MediaContext>>();
    let context = (*media_context.read()).clone();
    #[cfg(target_arch = "wasm32")]
    let links = use_context::<Signal<MediaLinkContext>>();
    #[cfg(target_arch = "wasm32")]
    let pump = use_store();
    #[cfg(target_arch = "wasm32")]
    let _revision = pump.revision()();
    #[cfg(target_arch = "wasm32")]
    let auth_generation = {
        let core = pump.core();
        let core = core.borrow();
        core.store().auth_generation
    };
    let preview = use_signal(|| Preview::Idle);
    let fetch_token = use_hook(|| Rc::new(Cell::new(0_u64)));
    #[cfg(target_arch = "wasm32")]
    let read_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    let read_token = Rc::clone(&fetch_token);
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive(
        (&path, &context, &auth_generation, &revision),
        move |(path, context, auth_generation, revision)| {
            let ticket = read_token.get().wrapping_add(1);
            read_token.set(ticket);
            if let Preview::Ready { url, .. } = &*preview.peek() {
                revoke_preview(url);
            }
            preview.set(Preview::Loading);
            load_media_file(
                read_pump.clone(),
                context,
                path,
                revision,
                auth_generation,
                ticket,
                Rc::clone(&read_token),
                preview,
            );
        },
    ));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (&alt, &link, &surface, &context);

    let drop_token = Rc::clone(&fetch_token);
    use_drop(move || {
        let ticket = drop_token.get().wrapping_add(1);
        drop_token.set(ticket);
        #[cfg(target_arch = "wasm32")]
        if let Preview::Ready { url, .. } = &*preview.peek() {
            revoke_preview(url);
        }
    });

    match &*preview.read() {
        Preview::Idle => rsx! {
            p { role: "status", class: "text-sm text-muted-foreground", "Loading media…" }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Loading => rsx! {
            p { role: "status", class: "text-sm text-muted-foreground", "Loading media…" }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Failed(error) => rsx! {
            p { role: "alert", class: "rounded-md border border-destructive p-3 text-sm text-destructive", "Media preview unavailable: {error}" }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Ready { url, mime } if mime.starts_with("image/") => rsx! {
            MediaImageView { url: url.clone(), alt: alt.clone(), link: link.clone(), surface: surface.clone(), transforms: links }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Ready { url, mime } if mime.starts_with("audio/") => rsx! {
            audio { class: "w-full", controls: true, preload: "metadata", src: "{url}", aria_label: "{alt}" }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Ready { url, mime } if mime.starts_with("video/") => rsx! {
            video { class: "max-h-96 w-full rounded-md bg-muted", controls: true, preload: "metadata", src: "{url}", aria_label: "{alt}" }
        },
        #[cfg(target_arch = "wasm32")]
        Preview::Ready { .. } => rsx! {
            p { role: "alert", class: "text-sm text-destructive", "Unsupported media type." }
        },
    }
}

#[cfg(target_arch = "wasm32")]
fn load_media_file(
    pump: Pump,
    context: MediaContext,
    raw_path: String,
    revision: Option<String>,
    generation: u64,
    ticket: u64,
    token: Rc<Cell<u64>>,
    preview: Signal<Preview>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(read_media_file(
        pump, context, raw_path, revision, generation, ticket, token, preview,
    ));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (pump, context, raw_path, revision, generation, ticket, token);
        let mut preview = preview;
        preview.set(Preview::Failed(
            "Media previews are available in the browser.".into(),
        ));
    }
}

#[cfg(target_arch = "wasm32")]
async fn read_media_file(
    pump: Pump,
    context: MediaContext,
    raw_path: String,
    revision: Option<String>,
    generation: u64,
    ticket: u64,
    token: Rc<Cell<u64>>,
    mut preview: Signal<Preview>,
) {
    let mime = match media_mime(&raw_path) {
        Some(mime) => mime,
        None => {
            preview.set(Preview::Failed(
                "This image, audio, or video format has no browser-safe preview.".into(),
            ));
            return;
        }
    };
    let worker_os = {
        let core = pump.core();
        let core = core.borrow();
        crate::terminal_href::worker_os(core.store(), &context.worker_fp).map(str::to_owned)
    };
    let Some(path) = resolve_worker_path(worker_os.as_deref(), &context.cwd, &raw_path) else {
        preview.set(Preview::Failed(
            "The media path could not be resolved within this worker's path rules.".into(),
        ));
        return;
    };
    let answer = pump
        .rpc()
        .call(&ReadFileRevision {
            worker_fp: context.worker_fp,
            path,
            revision: revision.unwrap_or_default(),
        })
        .await;
    if token.get() != ticket {
        return;
    }
    let still_current = {
        let core = pump.core();
        let core = core.borrow();
        captured_generation_is_current(core.store(), generation)
    };
    if !still_current {
        preview.set(Preview::Failed(
            "Authentication changed while loading this media. Retry after reconnecting.".into(),
        ));
        return;
    }
    match answer {
        Ok(contents) => {
            if contents.size > crate::components::file_viewer::state::MAX_VIEWED_FILE_BYTES {
                preview.set(Preview::Failed(format!(
                    "The file is {} bytes; previews are limited to {} bytes.",
                    contents.size,
                    crate::components::file_viewer::state::MAX_VIEWED_FILE_BYTES
                )));
                return;
            }
            if usize::try_from(contents.size).ok() != Some(contents.data.len()) {
                preview.set(Preview::Failed(
                    "The worker returned an incomplete media file.".into(),
                ));
                return;
            }
            if let Some(url) = preview_bytes(&contents.data, mime) {
                preview.set(Preview::Ready {
                    url,
                    mime: mime.to_owned(),
                });
            } else {
                preview.set(Preview::Failed(
                    "The browser refused this media preview.".into(),
                ));
            }
        }
        Err(error) => preview.set(Preview::Failed(error.to_string())),
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn media_mime(path: &str) -> Option<&'static str> {
    let extension = path
        .rsplit(['/', '\\'])
        .next()?
        .rsplit_once('.')?
        .1
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        "mp3" => Some("audio/mpeg"),
        "m4a" | "mp4a" => Some("audio/mp4"),
        "wav" => Some("audio/wav"),
        "ogg" | "oga" => Some("audio/ogg"),
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "ogv" => Some("video/ogg"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::media_mime;

    #[test]
    fn previews_only_browser_safe_media_extensions() {
        assert_eq!(media_mime("/tmp/photo.jpg"), Some("image/jpeg"));
        assert_eq!(media_mime("../clip.webm"), Some("video/webm"));
        assert_eq!(media_mime("/tmp/active.svg"), None);
        assert_eq!(media_mime("/tmp/document.pdf"), None);
    }
}
