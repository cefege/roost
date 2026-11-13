//! Accessible HTML rendering for OMP's semantic TSP surfaces.
//! Events are serialized unchanged to the parent transport; all command policy
//! and setting mutation remain in OMP's existing controllers.
mod actions;
mod body_preview;
mod chrome;
mod content;
mod controls;
mod editor;
mod editors;
mod elapsed_clock;
pub(crate) mod host_services;
mod hovercard;
mod key_hint;
mod keyboard;
mod layout;
pub(crate) mod links;
mod media;
#[cfg(target_arch = "wasm32")]
mod media_view;
mod native_text;
mod node_menu;
mod nodes;
mod overlay;
mod overlay_render;
#[cfg(test)]
mod overlay_tests;
mod picker;
mod prefs;
mod query;
mod regions;
mod scroll_effects;
mod text_edit;
mod text_state;
mod text_sync;
mod theme;
use media::MediaContext;
#[cfg(target_arch = "wasm32")]
use media::MediaLinkContext;
use nodes::{render_node, render_node_expanded, render_node_window, render_node_with_focus};
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiSurface};

use crate::pump::Pump;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::{Value, json};
#[derive(Clone, PartialEq)]
pub(super) struct NativeViewScope {
    pub view_id: std::rc::Rc<str>,
}

/// The DOM-scope id of one surface's regions inside a view; overlay placement
/// resolves its bounds through the element rendered with this id.
pub(super) fn surface_view_id(view_id: &str, surface_id: &str) -> String {
    format!("{view_id}-{surface_id}")
}

struct RegionContext<'view> {
    pump: &'view Pump,
    conversation_id: &'view str,
    view_id: &'view str,
    host_surface: Option<&'view str>,
    active_screen: Option<&'view str>,
    revision: u64,
}

#[component]
pub fn OmpCommandSurface(
    conversation_id: String,
    view_id: String,
    transport_id: String,
    worker_fp: String,
    cwd: String,
    pump: Pump,
    #[props(default)] input_failure: Option<(u64, String)>,
    on_action: EventHandler<Value>,
) -> Element {
    let _revision = pump.revision()();
    use_context_provider(|| NativeViewScope {
        view_id: std::rc::Rc::from(view_id.as_str()),
    });
    use_context_provider(hovercard::HoverCardScope::default);
    host_services::use_omp_host_services(
        pump.clone(),
        conversation_id.clone(),
        view_id.clone(),
        transport_id,
        worker_fp.clone(),
        cwd.clone(),
        on_action,
    );
    let input_error = use_signal(|| None::<String>);
    let input_sync = use_context_provider(|| text_sync::TextSync::new(on_action, input_error));
    input_sync.update_sender(on_action);
    let failure_sync = input_sync.clone();
    use_effect(use_reactive((&input_failure,), move |(failure,)| {
        if let Some((_, message)) = failure {
            failure_sync.input_failed(&message);
        }
    }));
    let on_action = use_callback(move |action: Value| input_sync.enqueue(action));
    let mut media_context = use_context_provider(|| {
        Signal::new(MediaContext {
            conversation_id: conversation_id.clone(),
            worker_fp: worker_fp.clone(),
            cwd: cwd.clone(),
        })
    });
    #[cfg(target_arch = "wasm32")]
    let mut media_links = use_context_provider(|| Signal::new(MediaLinkContext::default()));
    #[cfg(target_arch = "wasm32")]
    let mut linked_conversation = use_signal(|| conversation_id.clone());
    #[cfg(target_arch = "wasm32")]
    if *linked_conversation.peek() != conversation_id {
        media_links.set(MediaLinkContext::default());
        linked_conversation.set(conversation_id.clone());
    }
    if media_context.peek().conversation_id.as_str() != conversation_id.as_str()
        || media_context.peek().worker_fp.as_str() != worker_fp.as_str()
        || media_context.peek().cwd.as_str() != cwd.as_str()
    {
        media_context.set(MediaContext {
            conversation_id: conversation_id.clone(),
            worker_fp: worker_fp.clone(),
            cwd: cwd.clone(),
        });
    }
    elapsed_clock::use_clock();
    let root_id = use_hook(|| crate::components::ui::field::scoped_element_id("omp-surface"));
    #[cfg(target_arch = "wasm32")]
    {
        // A program overlay replaces the focused composer, and the browser then
        // parks focus on <body>, outside this subtree's key handler. The terminal
        // keeps delivering keys to the program, so reclaim focus when nothing owns it.
        let focus_root_id = root_id.clone();
        use_effect(use_reactive((&_revision,), move |_| {
            let Some(document) = web_sys::window().and_then(|window| window.document()) else {
                return;
            };
            let parked = document
                .active_element()
                .is_none_or(|active| active.tag_name().eq_ignore_ascii_case("body"));
            if parked
                && let Some(root) = document.get_element_by_id(&focus_root_id)
                && let Ok(root) = wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(root)
            {
                let _ = root.focus();
            }
        }));
    }
    let core = pump.core();
    let core = core.borrow();
    let document = core
        .store()
        .agent_chat
        .transcripts
        .get(&conversation_id)
        .and_then(|loaded| loaded.transcript.omp_ui.as_ref());
    let Some(document) = document else {
        return rsx! {host_services::OmpHostServicePrompts{} p{role:"status",class:"text-sm text-muted-foreground","Connecting to OMP…"}};
    };
    let active_screen = document
        .surfaces
        .values()
        .rfind(|surface| !surface.suspended && surface.mode == "screen")
        .map(|surface| surface.id.as_str());
    let has_open_modal = active_screen.is_some()
        || document
            .surfaces
            .values()
            .filter(|surface| !surface.suspended)
            .any(overlay::surface_has_modal);
    let host_surface = active_screen
        .or_else(|| {
            document
                .surfaces
                .values()
                .rfind(|surface| !surface.suspended && overlay::surface_has_modal(surface))
                .map(|surface| surface.id.as_str())
        })
        .or_else(|| {
            document
                .surfaces
                .values()
                .find(|surface| !surface.suspended)
                .map(|surface| surface.id.as_str())
        });
    let regions = RegionContext {
        pump: &pump,
        conversation_id: &conversation_id,
        view_id: &view_id,
        host_surface,
        active_screen,
        revision: _revision,
    };
    let key_action = on_action;
    let theme_style = theme::surface_style(&document.palette);
    rsx! {
        div { id: "{root_id}", tabindex: "-1", class: "relative flex h-full min-h-0 min-w-0 flex-1 flex-col outline-none",
            style: "{theme_style}",
            "data-omp-theme-scope": "true",
            onkeydown: move |event: KeyboardEvent| { if let Some(data)=keyboard::surface_input(&event,has_open_modal) { event.prevent_default();event.stop_propagation();key_action.call(json!({"method":"input","params":{"data":data}})); } },
            if let Some(error)=input_error(){p{role:"alert",class:"shrink-0 px-4 py-2 text-sm text-destructive","{error}"}}
            if host_surface.is_none(){host_services::OmpHostServicePrompts{}}
            for surface in document.surfaces.values().filter(|surface| !surface.suspended) {
                {
                    let covered = regions
                        .active_screen
                        .is_some_and(|active| active != surface.id.as_str());
                    let show_host_services = regions.host_surface == Some(surface.id.as_str());
                    rsx! {
                        {render_regions(
                            surface,
                            &on_action,
                            &document.blobs,
                            &regions,
                            covered,
                            show_host_services,
                        )}
                    }
                }
            }
        }
    }
}
fn render_regions(
    surface: &OmpUiSurface,
    send: &EventHandler<Value>,
    blobs: &std::collections::BTreeMap<String, OmpUiBlob>,
    context: &RegionContext<'_>,
    covered: bool,
    show_host_services: bool,
) -> Element {
    let main = surface.root.c.iter().find(|node| node.id == "main");
    let dock = surface.root.c.iter().find(|node| node.id == "dock");
    let layer = surface
        .root
        .c
        .iter()
        .find(|node| node.id == "layer")
        .filter(|node| !node.c.is_empty());
    let main = main.or_else(|| {
        if dock.is_none() && layer.is_none() {
            Some(&surface.root)
        } else {
            None
        }
    });
    let focused = surface.focus.as_deref();
    let main = main
        .map(|node| render_node_with_focus(node, &surface.id, send, blobs, focused))
        .unwrap_or_else(|| rsx! {});
    let dock = dock
        .map(|node| render_node_with_focus(node, &surface.id, send, blobs, focused))
        .unwrap_or_else(|| rsx! {});
    let overlays = overlay::layer_slots(layer, surface, send, blobs);
    let layer_marker = overlay::layer_marker(layer);
    rsx! {regions::NativeRegions{
        key:"{surface.id}",surface_id:surface.id.clone(),title:surface.title.clone().unwrap_or_else(||"OMP command".into()),
        surface_sequence:surface.sequence,
        conversation_id:context.conversation_id.to_owned(),view_id:surface_view_id(context.view_id,&surface.id),pump:context.pump.clone(),
        revision:context.revision,show_host_services,screen:surface.mode=="screen",covered,
        layer_marker,overlays,main,dock,send:*send
    }}
}

pub(super) fn line_count(node: &OmpUiNode) -> Option<usize> {
    content::line_count(node)
}

pub(super) fn safe_display(value: &Value) -> String {
    value.to_string()
}

pub(super) fn event(verb: &str, sf: &str, id: &str, mut fields: Value) -> Value {
    if let Some(object) = fields.as_object_mut() {
        object.insert("ev".into(), json!(verb));
        object.insert("sf".into(), json!(sf));
        object.insert("id".into(), json!(id));
    }
    json!({"method":"event","params":{"event":fields}})
}
pub(super) fn prop_text(props: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| {
            v.as_str().map(str::to_owned).or_else(|| {
                v.as_array().map(|spans| {
                    spans
                        .iter()
                        .filter_map(|span| span.get("t").and_then(Value::as_str))
                        .collect::<String>()
                })
            })
        })
        .filter(|s| !s.is_empty())
}
pub(super) fn text_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(spans) => spans
            .iter()
            .filter_map(|span| span.get("t").and_then(Value::as_str))
            .collect(),
        _ => safe_display(value),
    }
}
