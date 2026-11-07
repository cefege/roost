//! Conversation-only OMP model and thinking pins with inherited source labels.
//! Choices come only from the machine's ACP catalog, never native providers.
//! Mounted by the composer; the shared resolver supplies inheritance details.

use crate::pump::Pump;
use roost_protocol::wire::agent_chat::*;

pub(crate) fn source_label(source: &OmpSettingSource) -> &'static str {
    match source {
        OmpSettingSource::Conversation => "this chat",
        OmpSettingSource::Folder => "this folder",
        OmpSettingSource::Machine => "this machine",
        OmpSettingSource::Defaults => "your defaults",
        OmpSettingSource::Omp => "OMP's own config",
    }
}

pub(crate) fn choice_label(choice: &OmpChoice) -> String {
    match split_omp_model(&choice.value) {
        Some(model) => format!("{} · {}", choice.name, model.provider),
        None => choice.name.clone(),
    }
}

pub(crate) async fn fetch_settings(pump: Pump) -> Result<OmpSettingsView, String> {
    #[cfg(target_arch = "wasm32")]
    {
        pump.rpc()
            .call(&roost_client_core::client::rpc::calls::agent_chat::GetOmpSettings {})
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = pump;
        Err("OMP settings are available in the browser.".into())
    }
}
