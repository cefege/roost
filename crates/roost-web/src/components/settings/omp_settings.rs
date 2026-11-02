//! Central OMP-only defaults with machine and exact-folder overrides.
//! Settings writes update one scope and refetch authoritative scopes/catalogs.
//! Native agent settings and each machine's login remain separate.

use super::omp_scope;
use crate::{
    components::{
        agent_chat::{
            agent_alert::AgentAlert,
            omp_controls::{fetch_settings, source_label},
        },
        ui::{Button, ButtonSize, ButtonVariant, Select, SelectOption, Skeleton},
    },
    pump::{Pump, use_store},
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::*;

#[component]
pub fn OmpSettingsPane() -> Element {
    let pump = use_store();
    let mut scope = use_signal(|| OmpSettingsScope::Defaults);
    let mut settings = use_signal(|| None::<OmpSettingsView>);
    let mut error = use_signal(|| None::<String>);
    let saving = use_signal(|| false);
    let fetch_pump = pump.clone();
    use_effect(move || {
        let pump = fetch_pump.clone();
        spawn(async move {
            match fetch_settings(pump).await {
                Ok(view) => settings.set(Some(view)),
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let Some(view) = settings() else {
        return rsx! {div {"data-testid":"omp-settings-pane",
            if let Some(message) = error() {AgentAlert {test_id:"omp-settings-error","{message}"}} else {Skeleton {class:"h-24 w-full"}}
        }};
    };
    let scope_rows = omp_scope::scope_choices(&pump, &view);
    let scope_options = scope_rows
        .iter()
        .map(|(scope, label)| SelectOption::new(omp_scope::scope_id(scope), label))
        .collect();
    let selected = scope();
    let values = view
        .scopes
        .iter()
        .find(|row| row.scope == selected)
        .map(|row| row.values.clone())
        .unwrap_or_default();
    let inherited = omp_scope::inherited(&view, &selected);
    let catalog_empty = view
        .catalogs
        .iter()
        .filter(|catalog| match &selected {
            OmpSettingsScope::Defaults => true,
            OmpSettingsScope::Machine { worker_fp }
            | OmpSettingsScope::Folder { worker_fp, .. } => worker_fp == &catalog.worker_fp,
        })
        .all(|catalog| catalog.models.is_empty() && catalog.thinking_levels.is_empty());
    rsx! {
        div {class:"flex min-w-0 flex-col gap-4","data-testid":"omp-settings-pane",
            p {class:"text-sm text-muted-foreground","Only OMP chats use these settings. Each machine uses its own OMP login."}
            Select {label:"Scope",value:omp_scope::scope_id(&selected),options:scope_options,test_id:"omp-settings-scope",class:"max-w-full",disabled:saving(),
                on_change:move |id:String| {if let Some((next,_)) = scope_rows.iter().find(|(scope,_)| omp_scope::scope_id(scope) == id) {scope.set(next.clone());}}
            }
            for model in [true,false] {
                {
                    let current = if model {values.model.clone()} else {values.thinking_level.clone()};
                    let inherited_value = if model {inherited.model.clone()} else {inherited.thinking_level.clone()};
                    let label = if model {"Model"} else {"Thinking"};
                    let source = source_label(&inherited_value.source);
                    let inherited_text = inherited_value.value.unwrap_or_else(|| "OMP default".into());
                    let choices = omp_scope::choices(&view,&selected,model,current.as_deref());
                    let save_pump = pump.clone();let reset_pump = pump.clone();
                    let save_scope = selected.clone();let reset_scope = selected.clone();
                    let save_values = values.clone();let reset_values = values.clone();
                    rsx! {
                        div {class:"flex min-w-0 flex-col gap-1",
                            Select {label,value:current.clone().unwrap_or_default(),options:choices,disabled:saving(),class:"max-w-full",test_id:if model {"omp-settings-model"} else {"omp-settings-thinking"},
                                on_change:move |value:String| save_field(save_pump.clone(),OmpScopedSettings {scope:save_scope.clone(),values:save_values.clone()},model,(!value.is_empty()).then_some(value),settings,error,saving)
                            }
                            if current.as_ref().is_some_and(|value| !value.is_empty()) {
                                div {class:"flex items-center gap-2",
                                    span {class:"text-xs text-muted-foreground","Set here"}
                                    Button {variant:ButtonVariant::Ghost,size:ButtonSize::Sm,disabled:saving(),onclick:move |_| save_field(reset_pump.clone(),OmpScopedSettings {scope:reset_scope.clone(),values:reset_values.clone()},model,None,settings,error,saving),"Reset"}
                                }
                            } else {p {class:"text-xs text-muted-foreground","Inherited from {source}: {inherited_text}"}}
                        }
                    }
                }
            }
            if catalog_empty {
                p {class:"text-sm text-muted-foreground","Model choices appear after an OMP chat has been opened on that machine."}
            }
            if let Some(message) = error() {AgentAlert {test_id:"omp-settings-error","{message}"}}
        }
    }
}

fn save_field(
    pump: Pump,
    mut scoped: OmpScopedSettings,
    model: bool,
    value: Option<String>,
    mut settings: Signal<Option<OmpSettingsView>>,
    mut error: Signal<Option<String>>,
    mut saving: Signal<bool>,
) {
    if saving() {
        return;
    }
    if model {
        scoped.values.model = value;
    } else {
        scoped.values.thinking_level = value;
    }
    #[cfg(target_arch = "wasm32")]
    {
        saving.set(true);
        spawn(async move {
            let result = async {
                let scoped_json =
                    serde_json::to_string(&scoped).map_err(|error| error.to_string())?;
                pump.rpc()
                    .call(
                        &roost_client_core::client::rpc::calls::agent_chat::SetOmpSettings {
                            scoped_json,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                fetch_settings(pump).await
            }
            .await;
            match result {
                Ok(view) => {
                    settings.set(Some(view));
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
            saving.set(false);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (pump, scoped, &mut settings, &mut saving);
        error.set(Some("OMP settings are available in the browser.".into()));
    }
}
