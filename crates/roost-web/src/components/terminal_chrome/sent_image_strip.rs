//! The preview strip beside a terminal of the images this tab uploaded into
//! it: the program was handed only a path, so the operator sees what they sent
//! here. `upload` records an image once its path is committed; the composer
//! renders the strip; every removal releases its preview URL here, the one
//! owner of the URLs `Store::sent_images` holds.

use dioxus::prelude::*;
use roost_client_core::store::sent_images::SentImage;

use super::dom;
use crate::components::md::{IconButton, IconButtonSize};
use crate::components::notifications::store_write::write_store;
use crate::pump::{Pump, use_store};

/// Record a committed image upload with a preview URL of its own (the transfer
/// card revokes the one it was given when the card leaves).
pub async fn record_sent_image(
    pump: &Pump,
    session_id: &str,
    upload_id: &str,
    file: &web_sys::File,
    name: &str,
    path: &str,
) {
    let Some(preview_url) = dom::preview_url(file).await else {
        return;
    };
    let image = SentImage {
        id: upload_id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        preview_url,
    };
    let removed = write_store(pump, |store| {
        store.sent_images.record(session_id, image);
        store.sent_images.take_removed()
    });
    release(&removed);
}

fn dismiss(pump: &Pump, session_id: &str, id: &str) {
    let removed = write_store(pump, |store| {
        store.sent_images.dismiss(session_id, id);
        store.sent_images.take_removed()
    });
    release(&removed);
}

fn release(removed: &[SentImage]) {
    for image in removed {
        dom::revoke_preview(&image.preview_url);
    }
}

/// The strip, or nothing when this tab sent the session no image.
#[component]
pub fn SentImageStrip(session_id: String) -> Element {
    let pump = use_store();
    let _ = pump.revision().read();
    let images = pump
        .core()
        .borrow()
        .store()
        .sent_images
        .for_session(&session_id);
    if images.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "sent-image-strip",
            "data-testid": "sent-image-strip",
            "aria-label": "Images sent to this terminal",
            for image in images {
                div {
                    key: "{image.id}",
                    class: "sent-image",
                    title: "{image.name} — {image.path}",
                    a {
                        href: image.preview_url.clone(),
                        target: "_blank",
                        rel: "noopener",
                        img { src: image.preview_url.clone(), alt: image.name.clone() }
                    }
                    IconButton {
                        icon: "close",
                        label: "Dismiss preview",
                        size: IconButtonSize::IconSm,
                        class: "sent-image__dismiss",
                        onclick: {
                            let pump = pump.clone();
                            let session_id = session_id.clone();
                            let id = image.id.clone();
                            move |_| dismiss(&pump, &session_id, &id)
                        },
                    }
                }
            }
        }
    }
}
