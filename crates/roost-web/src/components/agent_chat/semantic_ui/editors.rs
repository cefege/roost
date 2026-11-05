//! Native editors and media leaves.
//!
//! Editors hand their text, caret and acknowledgement to the one text-edit
//! controller; media resolves through the authenticated preview surface. Both
//! are leaves: their enclosing node wrapper owns the wire anchor and any
//! declared pointer action.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::Value;

use super::editor::OmpEditor;
use super::media::MediaFilePreview;
use super::{prop_text, text_value};

pub(super) fn edit_node(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    focused: bool,
) -> Element {
    let text = prop_text(&node.p, "text").unwrap_or_default();
    let id = node.id.clone();
    let surface = surface.to_owned();
    let readonly = node
        .p
        .get("readonly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sendable = node
        .p
        .get("sendable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let typeahead = node
        .p
        .get("typeahead")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cursor = node
        .p
        .get("cursor")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let anchor = node
        .p
        .get("anchor")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let placeholder = prop_text(&node.p, "placeholder").unwrap_or_default();
    let prompt = node.p.get("prompt").map(text_value);
    let ghost = node
        .p
        .get("ghost")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let single_line = node.k == "input";
    let max_lines = node
        .p
        .get("maxLines")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(18);
    let secret = node.p.get("mask").and_then(Value::as_bool).unwrap_or(false);
    let edit_ack = node.p.get("ack").cloned();
    rsx! {
        OmpEditor {
            key: "{surface}/{id}",
            id,
            surface,
            text,
            readonly,
            sendable,
            typeahead,
            focused,
            cursor,
            anchor,
            placeholder,
            prompt,
            ghost,
            single_line,
            max_lines,
            secret,
            edit_ack,
            send: *send
        }
    }
}

pub(super) fn media_node(
    node: &OmpUiNode,
    surface: &str,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    let alt = prop_text(&node.p, "alt").unwrap_or_else(|| "Media supplied by OMP".into());
    let blob = node
        .p
        .get("blob")
        .and_then(Value::as_str)
        .and_then(|id| blobs.get(id));
    if node.k == "media" {
        if let Some(path) = prop_text(&node.p, "path") {
            return rsx! {
                MediaFilePreview {
                    path,
                    alt,
                    revision: prop_text(&node.p, "rev"),
                    link: prop_text(&node.p, "link"),
                    surface: surface.to_owned()
                }
            };
        }
        return rsx! { p { role: "alert", class: "text-sm text-destructive", "OMP media is missing a file path." } };
    }
    if node.k == "image" {
        if node.p.get("builtin").and_then(Value::as_str) == Some("omp") {
            let width = node
                .p
                .get("w")
                .and_then(Value::as_u64)
                .unwrap_or(120)
                .clamp(16, 512);
            return rsx! {
                img {
                    class: "max-h-24 max-w-full object-contain",
                    src: "/omp-mark.svg",
                    width: "{width}",
                    alt: "{alt}",
                    loading: "lazy"
                }
            };
        }
        if let Some(blob) = blob.filter(|blob| {
            blob.data_base64.len() <= 4 * 1024 * 1024
                && matches!(
                    blob.mime.as_str(),
                    "image/png"
                        | "image/jpeg"
                        | "image/webp"
                        | "image/gif"
                        | "image/avif"
                        | "image/bmp"
                )
        }) {
            let source = format!("data:{};base64,{}", blob.mime, blob.data_base64);
            return rsx! { img { class: "max-h-96 max-w-full rounded-md object-contain", src: "{source}", alt: "{alt}", loading: "lazy" } };
        }
        if blob.is_some() {
            return rsx! { p { role: "alert", class: "text-sm text-destructive", "OMP image is unsupported or exceeds the preview limit: {alt}" } };
        }
    }
    rsx! { p { role: "alert", class: "text-sm text-destructive", "OMP image is unavailable: {alt}" } }
}
