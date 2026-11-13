//! Exercises the real preview header and body through registry and read completion.
//! The conformance vector changes both download controls and content in one diff.
//! This catches mount-shape corruption that static preview mounts cannot exercise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use dioxus::core::Mutation;
use roost_client_core::ClientCore;
use serde::Deserialize;
use std::cell::RefCell;

#[derive(Clone, PartialEq, Deserialize)]
struct PreviewVector {
    path: String,
    worker_fp: String,
    byte_size: u64,
    lines: Vec<String>,
    steps: Vec<PreviewStage>,
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreviewStage {
    Pending,
    Loading,
    Text,
}

#[derive(Clone, PartialEq, Props)]
struct PreviewProps {
    stage_slot: Rc<RefCell<Option<Signal<usize>>>>,
    vector: Rc<PreviewVector>,
}

fn preview(props: PreviewProps) -> Element {
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("preview-transition"))),
            Signal::new(0_u64),
            Rc::new(crate::platform::connect::CoordRpc::new(
                "http://127.0.0.1:4113",
                "",
            )),
        )
    });
    let stage = use_signal(|| 0_usize);
    *props.stage_slot.borrow_mut() = Some(stage);
    let current_stage = props.vector.steps[stage()];
    let current = match current_stage {
        PreviewStage::Pending => ViewerContent::Idle,
        PreviewStage::Loading => ViewerContent::Loading,
        PreviewStage::Text => ViewerContent::Text {
            lines: props.vector.lines.clone(),
            tokens: None,
            byte_size: props.vector.byte_size,
        },
    };
    rsx! {
        Dialog { open: true, on_open_change: move |_| {},
            DialogContent {
                DialogHeader { DialogTitle { "File preview" } }
                {header_row(&props.vector.path, &props.vector.worker_fp, current.is_downloadable(), current.byte_size())}
                if current_stage != PreviewStage::Text { states::LoadingCaption {} }
                if current_stage != PreviewStage::Pending {
                    match current {
                        ViewerContent::Text { lines, tokens, .. } => rsx! {
                            FileBody { lines, tokens, target_line: 0, copied_line: None, on_copy: move |_| {} }
                        },
                        _ => rsx! {},
                    }
                }
            }
        }
    }
}

#[test]
fn loaded_preview_paints_file_lines_when_download_controls_appear() {
    let vector: Rc<PreviewVector> = Rc::new(
        serde_json::from_str(include_str!(
            "../../../../../protocol/conformance/dom-placement/file-preview-loading.json"
        ))
        .expect("preview vector"),
    );
    let slot = Rc::new(RefCell::new(None));
    let mut dom = VirtualDom::new_with_props(
        preview,
        PreviewProps {
            stage_slot: Rc::clone(&slot),
            vector: Rc::clone(&vector),
        },
    );
    dom.rebuild_in_place();
    let mut stage = slot.borrow().expect("preview stage");
    let mut painted = Vec::new();
    for index in 1..vector.steps.len() {
        dom.in_runtime(|| stage.set(index));
        dom.mark_dirty(ScopeId::ROOT);
        painted = dom
            .render_immediate_to_vec()
            .edits
            .into_iter()
            .filter_map(|edit| match edit {
                Mutation::CreateText { value } | Mutation::SetText { value, .. } => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>();
    }
    for line in &vector.lines {
        assert!(
            painted.contains(line),
            "missing visible file line {line:?}: {painted:?}"
        );
    }
}
