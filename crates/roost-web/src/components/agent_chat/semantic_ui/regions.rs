//! A native surface's scrolling history, fixed composer dock and overlay stack.
//! Mounted history owns follow-tail state; native updates never pull a reader
//! back down after they scroll away from the latest output. Layer children are
//! individual overlays: non-modal ones float without trapping focus, while a
//! modal or screen is a stock Dialog above the covered surface.

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::components::ui::{Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle};
use crate::pump::Pump;

use super::host_services::OmpHostServicePrompts;
use super::overlay::{self, NativeLayerMarker, OverlaySlot};
use super::overlay_render;

#[component]
pub(super) fn NativeRegions(
    surface_id: String,
    conversation_id: String,
    view_id: String,
    pump: Pump,
    show_host_services: bool,
    screen: bool,
    covered: bool,
    title: String,
    revision: u64,
    layer_marker: Option<NativeLayerMarker>,
    overlays: Vec<OverlaySlot>,
    main: Element,
    dock: Element,
    send: EventHandler<Value>,
) -> Element {
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let mut follow_tail = use_signal(|| true);
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive((&revision,), move |_| {
        if *follow_tail.peek() {
            if let Some(mounted) = mounted.peek().as_ref() {
                super::super::surface_dom::scroll_to_bottom(mounted);
            }
        }
    }));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = revision;

    let has_modal_layer = overlay::has_modal_layer(&overlays);
    let hidden_content = covered || has_modal_layer;
    let prompts_root = show_host_services && !covered && !has_modal_layer && !screen;
    let prompts_screen = show_host_services && !covered && !has_modal_layer && screen;
    let dock_overlays =
        overlay_render::non_modal_dock(&overlays, &surface_id, covered || has_modal_layer);
    let surface_overlays =
        overlay_render::non_modal_surface(&overlays, &surface_id, covered || has_modal_layer);
    let modals = if covered {
        Vec::new()
    } else {
        overlay_render::modal_dialogs(&overlays, show_host_services, send)
    };

    let content = rsx! {
        div {
            class: "relative flex min-h-0 min-w-0 flex-1 flex-col",
            inert: hidden_content.then_some("true"),
            aria_hidden: hidden_content.to_string(),
            if prompts_root {
                OmpHostServicePrompts {}
            }
            if let Some(marker) = &layer_marker {
                {overlay_render::layer_marker_element(marker, &surface_id)}
            }
            div {
                class: "min-h-0 flex-1 overflow-y-auto overscroll-y-contain",
                "data-omp-history": surface_id.clone(),
                onmounted: move |event| {
                    let element = event.data();
                    #[cfg(target_arch = "wasm32")]
                    super::super::surface_dom::scroll_to_bottom(&element);
                    mounted.set(Some(element));
                    follow_tail.set(true);
                },
                onscroll: move |_| {
                    #[cfg(target_arch = "wasm32")]
                    if let Some(element) = mounted.peek().as_ref() {
                        if let Some(distance) = super::super::surface_dom::distance_from_bottom(element) {
                            follow_tail.set(distance <= 64);
                        }
                    }
                },
                div { class: "mx-auto w-full max-w-reading min-w-0 px-4 py-3", {main} }
            }
            div {
                class: "relative mx-auto w-full max-w-reading min-w-0 max-h-full shrink-0",
                "data-omp-dock": true,
                {dock_overlays}
                div { class: "max-h-full overflow-y-auto border-t border-border px-4 pt-2 pb-3 flex flex-col-reverse", {dock} }
            }
            {surface_overlays}
        }
    };

    let body = if screen && !covered && !has_modal_layer {
        rsx! {
            Dialog {
                open: true,
                on_open_change: move |open: bool| {
                    if !open {
                        send.call(json!({"method":"input","params":{"data":"\x1b"}}));
                    }
                },
                DialogContent {
                    class: Some("flex min-h-0 flex-col overflow-auto pt-12".into()),
                    style: Some("position:absolute;height:100%;".into()),
                    fullscreen: true,
                    close_on_backdrop_click: true,
                    DialogHeader { class: Some("sr-only".into()),
                        DialogTitle { "{title}" }
                        DialogDescription { class: Some("sr-only".into()), "OMP command surface" }
                    }
                    if prompts_screen {
                        OmpHostServicePrompts {}
                    }
                    {content}
                }
            }
        }
    } else {
        content
    };

    let surface_class = if screen && covered {
        "hidden"
    } else if screen {
        "absolute inset-0 z-10 flex h-full min-h-0 min-w-0 flex-col bg-background"
    } else {
        "relative flex h-full min-h-0 min-w-0 flex-1 flex-col"
    };

    rsx! {
        div {
            id: "omp-view-{view_id}",
            "data-omp-surface": surface_id.clone(),
            class: "{surface_class}",
            {body}
            for modal in modals {
                {modal}
            }
            super::scroll_effects::NativeScrollEffects {
                pump,
                conversation_id,
                surface_id: surface_id.clone(),
                view_id: view_id.clone(),
                revision,
                on_follow: move |follow| follow_tail.set(follow),
            }
        }
    }
}
