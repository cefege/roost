//! The models hub that `/model` and `/models` open, ported from OMP's
//! `model-hub.ts`: a sidebar of views (Roles, Recent, All models, providers)
//! beside the content in a dialog. Roles mounts the role editor; the model views
//! browse rows filtered by kind and search. Enter on a model opens its role strip to
//! assign it to a role or switch this chat's model. A provider not signed in offers
//! its sign-in. Mounted by `agent_chat::surface`.
use std::{cell::RefCell, rc::Rc};

mod hub_browse;
mod hub_catalog;
mod hub_format;
mod hub_keys;
mod hub_list;
pub mod hub_logic;
mod hub_order;
mod hub_text;
mod locked_provider;
mod model_hub_actions;
mod model_hub_keys;
mod model_role_strip;
mod roles;
mod strip_chips;
mod strip_thinking;

mod content;

use dioxus::prelude::*;
use libero::components::Dialog;
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog, ProviderEntry};

use self::content::ModelHubContent;
use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::Pump;

#[component]
pub fn ModelHub(
    open: Signal<bool>,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let dialog_sx = use_hook(|| {
        sx().width("min(calc(100vw - 2rem), 64rem)")
            .height("min(85dvh, 56rem)")
            .max_width("none")
            .padding("0")
            .display("flex")
            .flex_direction("column")
    });
    let generations = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let mut active_identity = use_signal(|| None::<String>);
    let mut request_generation = use_signal(|| 0_u64);
    let request_identity = conversation.id.clone();
    let modal = use_modal(move |_scope: ModalScope<()>| {
        rsx! {
            Dialog { title: "Models", size: "64rem", sx: dialog_sx.clone(),
                ModelHubContent {
                    key: "{request_generation()}",
                    open,
                    request_generation,
                    conversation: conversation.clone(),
                    catalog: catalog.clone(),
                    pump: pump.clone(),
                    connect_provider,
                }
            }
        }
    });
    let teardown_generations = generations.clone();
    use_drop(move || teardown_generations.borrow_mut().invalidate());
    use_effect(use_reactive!(|request_identity| {
        if open() {
            if active_identity.peek().as_deref() != Some(request_identity.as_str()) {
                generations.borrow_mut().invalidate();
                modal.close();
                let generation = generations.borrow_mut().begin();
                active_identity.set(Some(request_identity.clone()));
                request_generation.set(generation);
                let completion_generations = generations.clone();
                let mut open_flag = open;
                modal.open().onresult(move |_| {
                    let current = completion_generations.borrow_mut().finish(generation);
                    if current {
                        open_flag.set(false);
                    }
                });
            }
        } else {
            active_identity.set(None);
            generations.borrow_mut().invalidate();
            if modal.is_open() {
                modal.close();
            }
        }
    }));
    rsx! {}
}
