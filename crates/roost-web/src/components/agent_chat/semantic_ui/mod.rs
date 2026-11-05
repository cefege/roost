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
pub(crate) mod host_services;
mod hovercard;
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
    #[props(default = false)] pending_input: bool,
    #[props(default = 0)] completed_input: u64,
    #[props(default)] input_failure: Option<(u64, String)>,
    on_action: EventHandler<Value>,
) -> Element {
    let _revision = pump.revision()();
    use_context_provider(|| NativeViewScope {
        view_id: std::rc::Rc::from(view_id.as_str()),
    });
    host_services::use_omp_host_services(
        pump.clone(),
        conversation_id.clone(),
        view_id.clone(),
        transport_id,
        worker_fp.clone(),
        cwd.clone(),
    );
    let input_error = use_signal(|| None::<String>);
    let mut pending_state = use_signal(|| pending_input);
    if *pending_state.peek() != pending_input {
        pending_state.set(pending_input);
    }
    let mut completed_state = use_signal(|| completed_input);
    if *completed_state.peek() != completed_input {
        completed_state.set(completed_input);
    }
    let input_sync = use_context_provider(|| {
        text_sync::TextSync::new(on_action, input_error, pending_state, completed_state)
    });
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
    if media_context.peek().worker_fp.as_str() != worker_fp.as_str()
        || media_context.peek().cwd.as_str() != cwd.as_str()
    {
        media_context.set(MediaContext {
            worker_fp: worker_fp.clone(),
            cwd: cwd.clone(),
        });
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
    rsx! {
        div { class: "relative flex h-full min-h-0 min-w-0 flex-1 flex-col",
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
        conversation_id:context.conversation_id.to_owned(),view_id:format!("{}-{}",context.view_id,surface.id),pump:context.pump.clone(),
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
