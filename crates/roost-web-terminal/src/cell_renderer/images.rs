//! Inline image layers and their on-demand object URLs.
//!
//! `CellGridRenderer` paints images from the same held frame as its text rows;
//! callers fetch missing content through `wanted_image_keys` and install PNGs.

use roost_protocol::cell::{CellGridFrame, ImagePlacement};

use crate::cell_renderer::CellGridRenderer;
use crate::image_geometry::image_style;
use crate::render_element::RenderElement;

impl<E: RenderElement> CellGridRenderer<E> {
    /// Image keys referenced by the painted frame that still need their PNG.
    pub fn wanted_image_keys(&self) -> Vec<u64> {
        let Some(frame) = self.frame.as_deref() else {
            return Vec::new();
        };
        let mut wanted = Vec::new();
        for placement in frame.image_placements.as_deref().unwrap_or_default() {
            let key = placement.image_key;
            if !self.image_urls.contains_key(&key)
                && !self.image_failed.contains(&key)
                && !wanted.contains(&key)
            {
                wanted.push(key);
            }
        }
        wanted.sort_unstable();
        wanted
    }

    /// Keep a successfully fetched PNG as an object URL and repaint its placements.
    pub fn install_image(&mut self, key: u64, png: &[u8]) {
        let Some(url) = create_image_object_url(key, png) else {
            self.image_failed(key);
            return;
        };
        if let Some(previous) = self.image_urls.insert(key, url) {
            revoke_image_object_url(&previous);
        }
        self.image_failed.remove(&key);
        self.paint_images();
    }

    /// Stop requesting an image that its carrier could not provide.
    pub fn image_failed(&mut self, key: u64) {
        self.image_failed.insert(key);
        if let Some(url) = self.image_urls.remove(&key) {
            revoke_image_object_url(&url);
        }
        self.paint_images();
    }

    /// Put the image layers back in their invariant viewport order.
    pub(crate) fn attach_image_layers(&self) {
        let first = self.viewport.first_child();
        if first.as_ref() != Some(&self.image_below) {
            self.viewport
                .insert_before(&self.image_below, first.as_ref());
        }
        self.viewport.append_child(&self.images);
    }

    /// Paint the image placements visible in the retained viewport window.
    pub(crate) fn paint_images(&mut self) {
        self.image_below.clear_children();
        self.images.clear_children();

        let frame = self.frame.clone();
        if let Some(frame) = frame.as_deref() {
            let retained_start = u64::from(self.painted_sb_base);
            let viewport_end = frame.scrollback_total.saturating_add(u64::from(frame.rows));
            for placement in frame.image_placements.as_deref().unwrap_or_default() {
                if !placement_intersects(placement, retained_start, viewport_end) {
                    continue;
                }
                let Some(url) = self.image_urls.get(&placement.image_key) else {
                    continue;
                };
                let Ok(image) = self.viewport.create_element("div") else {
                    continue;
                };
                paint_one_image(&image, placement, frame.scrollback_total, url);
                let layer = if placement.z_index < 0 {
                    &self.image_below
                } else {
                    &self.images
                };
                layer.append_child(&image);
            }
        }
        self.release_unreferenced_images(frame.as_deref());
    }

    fn release_unreferenced_images(&mut self, frame: Option<&CellGridFrame>) {
        self.image_failed
            .retain(|image_key| frame_references_image(frame, *image_key));
        self.image_urls.retain(|image_key, url| {
            let referenced = frame_references_image(frame, *image_key);
            if !referenced {
                revoke_image_object_url(url);
            }
            referenced
        });
    }
}

fn frame_references_image(frame: Option<&CellGridFrame>, image_key: u64) -> bool {
    frame.is_some_and(|frame| {
        frame
            .image_placements
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|placement| placement.image_key == image_key)
    })
}

fn placement_intersects(
    placement: &ImagePlacement,
    retained_start: u64,
    viewport_end: u64,
) -> bool {
    let placement_end = placement.row.saturating_add(u64::from(placement.rows));
    placement.row < viewport_end && placement_end > retained_start
}

fn paint_one_image<E: RenderElement>(
    image: &E,
    placement: &ImagePlacement,
    scrollback_total: u64,
    url: &str,
) {
    let style = image_style(placement, scrollback_total);
    image.add_class("cell-image");
    image.set_attribute("aria-hidden", "true");
    image.set_style("top", &style.top);
    image.set_style("left", &style.left);
    image.set_style("width", &style.width);
    image.set_style("height", &style.height);
    image.set_style("background-size", &style.background_size);
    image.set_style("background-position", &style.background_position);
    image.set_style("z-index", &style.z_index.to_string());
    image.set_style("background-image", &format!("url(\"{url}\")"));
}

#[cfg(target_arch = "wasm32")]
fn create_image_object_url(_key: u64, png: &[u8]) -> Option<String> {
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("image/png");
    let bytes: wasm_bindgen::JsValue = js_sys::Uint8Array::from(png).into();
    let parts = js_sys::Array::of1(&bytes);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn create_image_object_url(key: u64, _png: &[u8]) -> Option<String> {
    Some(format!("test://{key}"))
}

#[cfg(target_arch = "wasm32")]
fn revoke_image_object_url(url: &str) {
    let _ = web_sys::Url::revoke_object_url(url);
}

#[cfg(not(target_arch = "wasm32"))]
fn revoke_image_object_url(_url: &str) {}
