//! Browser image viewports with native same-surface pan/zoom linking.
//! Pointer contacts are owned by the mounted image; only active contacts move it.
//! Shared transforms remain conversation-owned and independent of file fetches.
use super::media::{MediaLinkContext, MediaViewTransform};
use crate::components::ui::{IconButton, IconType};
use dioxus::prelude::*;

#[derive(Clone, Copy)]
struct ImageViewport {
    links: Signal<MediaLinkContext>,
    local: Signal<MediaViewTransform>,
    key: Memo<Option<(String, String)>>,
}
impl ImageViewport {
    fn current(self) -> MediaViewTransform {
        self.key
            .read()
            .as_ref()
            .and_then(|key| self.links.read().0.get(key).copied())
            .unwrap_or_else(|| (self.local)())
    }
    fn set(mut self, next: MediaViewTransform) {
        if let Some(key) = self.key.read().as_ref() {
            self.links.write().0.insert(key.clone(), next);
        } else {
            self.local.set(next);
        }
    }
    fn zoom(self, factor: f64) {
        let current = self.current();
        self.set(MediaViewTransform {
            scale: (current.scale * factor).clamp(1.0, 8.0),
            ..current
        });
    }
}

#[derive(Clone, Copy, Default)]
struct Contacts([Option<(i32, (f64, f64))>; 2]);
impl Contacts {
    fn begin(&mut self, id: i32, point: (f64, f64)) {
        if let Some(slot) = self
            .0
            .iter_mut()
            .find(|slot| slot.is_none() || slot.is_some_and(|entry| entry.0 == id))
        {
            *slot = Some((id, point));
        }
    }
    fn end(&mut self, id: i32) {
        for slot in &mut self.0 {
            if slot.is_some_and(|entry| entry.0 == id) {
                *slot = None;
            }
        }
    }
    fn moved(
        &mut self,
        id: i32,
        point: (f64, f64),
        current: MediaViewTransform,
    ) -> Option<MediaViewTransform> {
        let slot = self
            .0
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.0 == id))?;
        let previous = self.0[slot]?.1;
        self.0[slot] = Some((id, point));
        let other = self
            .0
            .iter()
            .flatten()
            .find(|entry| entry.0 != id)
            .map(|entry| entry.1);
        let (scale, divisor) = if let Some(other) = other {
            let old_distance = (previous.0 - other.0).hypot(previous.1 - other.1).max(1.0);
            let distance = (point.0 - other.0).hypot(point.1 - other.1).max(1.0);
            (
                (current.scale * distance / old_distance).clamp(1.0, 8.0),
                2.0,
            )
        } else {
            (current.scale, 1.0)
        };
        Some(MediaViewTransform {
            scale,
            x: if scale > 1.0 {
                current.x + (point.0 - previous.0) / divisor
            } else {
                0.0
            },
            y: if scale > 1.0 {
                current.y + (point.1 - previous.1) / divisor
            } else {
                0.0
            },
        })
    }
}

#[component]
pub(super) fn MediaImageView(
    url: String,
    alt: String,
    link: Option<String>,
    surface: String,
    transforms: Signal<MediaLinkContext>,
) -> Element {
    let local = use_signal(MediaViewTransform::default);
    let key = use_memo(use_reactive((&link, &surface), |(link, surface)| {
        link.map(|link| (surface, link))
    }));
    let viewport = ImageViewport {
        links: transforms,
        local,
        key,
    };
    let current = viewport.current();
    let style = format!(
        "transform: translate({}px, {}px) scale({});",
        current.x, current.y, current.scale
    );
    let mut contacts = use_signal(Contacts::default);
    rsx! {
        div { class: "flex min-w-0 flex-col gap-2",
            div { class: "flex items-center gap-1",
                IconButton { icon: IconType::ZoomIn, label: "Zoom in", title: "Zoom in", onclick: move |_| viewport.zoom(1.25) }
                IconButton { icon: IconType::ZoomOut, label: "Zoom out", title: "Zoom out", onclick: move |_| viewport.zoom(0.8) }
                IconButton { icon: IconType::RotateCcw, label: "Reset zoom", title: "Reset zoom", onclick: move |_| viewport.set(MediaViewTransform::default()) }
            }
            div { class: "max-h-96 overflow-hidden rounded-md bg-muted touch-none",
                onwheel: move |event: WheelEvent| {
                    let delta = event.delta().strip_units().y;
                    if delta != 0.0 {
                        event.prevent_default();
                        viewport.zoom(if delta < 0.0 { 1.1 } else { 1.0/1.1 });
                    }
                },
                onpointerdown: move |event: PointerEvent| {
                    let point = event.client_coordinates();
                    contacts.write().begin(event.pointer_id(), (point.x, point.y));
                    capture_pointer(&event);
                },
                onpointerup: move |event: PointerEvent| contacts.write().end(event.pointer_id()),
                onpointercancel: move |event: PointerEvent| contacts.write().end(event.pointer_id()),
                onlostpointercapture: move |event: PointerEvent| contacts.write().end(event.pointer_id()),
                onpointermove: move |event: PointerEvent| {
                    let point = event.client_coordinates();
                    let next = contacts.write().moved(event.pointer_id(), (point.x, point.y), viewport.current());
                    if let Some(next) = next { viewport.set(next); }
                },
                img { class: "max-h-96 max-w-full select-none object-contain", style, src: url, alt, draggable: "false", loading: "lazy" }
            }
        }
    }
}

fn capture_pointer(event: &PointerEvent) {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt as _;
        use wasm_bindgen::JsCast as _;
        if let Some(native) = event.try_as_web_event() {
            let native: web_sys::PointerEvent = native;
            if let Some(target) = native
                .current_target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            {
                let _ = target.set_pointer_capture(native.pointer_id());
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = event;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hover_cannot_begin_a_drag_and_released_contacts_cannot_pan() {
        let mut contacts = Contacts::default();
        let current = MediaViewTransform {
            scale: 2.0,
            ..Default::default()
        };
        assert_eq!(contacts.moved(7, (20.0, 30.0), current), None);
        contacts.begin(7, (10.0, 10.0));
        assert_eq!(
            contacts.moved(7, (20.0, 30.0), current),
            Some(MediaViewTransform {
                scale: 2.0,
                x: 10.0,
                y: 20.0
            })
        );
        contacts.end(7);
        assert_eq!(contacts.moved(7, (50.0, 50.0), current), None);
    }
    #[test]
    fn pinch_changes_zoom_and_reset_restores_a_visible_image() {
        let mut contacts = Contacts::default();
        contacts.begin(1, (0.0, 0.0));
        contacts.begin(2, (10.0, 0.0));
        let result = contacts.moved(2, (20.0, 0.0), MediaViewTransform::default());
        assert_eq!(
            result,
            Some(MediaViewTransform {
                scale: 2.0,
                x: 5.0,
                y: 0.0
            })
        );
        assert_eq!(
            MediaViewTransform::default(),
            MediaViewTransform {
                scale: 1.0,
                x: 0.0,
                y: 0.0
            }
        );
    }
}
