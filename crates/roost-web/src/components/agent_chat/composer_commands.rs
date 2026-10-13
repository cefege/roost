//! How a composer command reaches the coordinator and how its outcome comes back:
//! typed slash commands and the composer chips share these, so a failure shows
//! the same inline alert whichever way the command was sent. Client-only
//! commands run here too. Called by `agent_chat::composer`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, ProviderEntry, SlashCommand, Transcript,
};

use crate::pump::Pump;
use crate::routes::Route;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::SubmitAgentChat;

/// Reports a command's result: a failure becomes an inline alert above the composer.
pub(super) fn report_command(
    mut command_error: Signal<Option<String>>,
) -> EventHandler<Result<(), String>> {
    EventHandler::new(move |result: Result<(), String>| command_error.set(result.err()))
}

/// The advisor chip's reader-facing state; a pending change says so instead of
/// reporting the old state.
pub(super) fn advisor_title(enabled: bool, pending: bool) -> &'static str {
    match (pending, enabled) {
        (true, _) => "Updating the advisor…",
        (false, true) => "Advisor is on. Select to turn it off.",
        (false, false) => "Advisor is off. Select to turn it on.",
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn submit_direct_command(
    pump: Pump,
    conversation_id: String,
    command: String,
    outcome: EventHandler<Result<(), String>>,
) {
    submit_message(pump, conversation_id, command, outcome);
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn submit_direct_command(
    _: Pump,
    _: String,
    _: String,
    _: EventHandler<Result<(), String>>,
) {
}

#[cfg(target_arch = "wasm32")]
pub(super) fn submit_message(
    pump: Pump,
    conversation_id: String,
    text: String,
    outcome: EventHandler<Result<(), String>>,
) {
    let mut bytes = [0_u8; 16];
    if let Some(crypto) = web_sys::window().and_then(|window| window.crypto().ok()) {
        let _ = crypto.get_random_values_with_u8_array(&mut bytes);
    }
    let request_id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let label = text.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match pump
            .rpc()
            .call(&SubmitAgentChat {
                conversation_id,
                text,
                request_id,
                ..Default::default()
            })
            .await
        {
            Ok(_) => outcome.call(Ok(())),
            Err(error) => {
                tracing::warn!(target: "agent_chat", %error, "agent message submit failed");
                outcome.call(Err(format!("Could not run {label}: {error}")));
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn submit_message(_: Pump, _: String, _: String, _: EventHandler<Result<(), String>>) {}

pub(super) struct LocalCommandScope<'a> {
    pub(super) conversation: &'a ConversationSummary,
    pub(super) transcript: &'a Transcript,
    pub(super) catalog: Option<&'a ModelsCatalog>,
    pub(super) pump: Pump,
    pub(super) navigate: EventHandler<String>,
    pub(super) connect_provider: Signal<Option<ProviderEntry>>,
}

/// Runs a client-side command; `false` means the text goes to the harness.
pub(super) fn run_local_command(
    command: &SlashCommand,
    args: &str,
    scope: &LocalCommandScope<'_>,
) -> bool {
    let (name, client_bare) = (command.name, command.client_bare);
    let conversation = scope.conversation;
    let catalog = scope.catalog;
    let pump = scope.pump.clone();
    let navigate = scope.navigate;
    let mut connect_provider = scope.connect_provider;
    match name {
        "model" if client_bare && args.is_empty() => {
            #[cfg(target_arch = "wasm32")]
            open_model_picker(&conversation.id);
            true
        }
        "settings" if client_bare => {
            navigate.call(Route::Settings { pane: None }.to_path());
            true
        }
        "login" => {
            let provider = catalog.and_then(|catalog| {
                if args.is_empty() {
                    catalog.providers.first()
                } else {
                    catalog
                        .providers
                        .iter()
                        .find(|provider| provider.id == args)
                }
            });
            if let Some(provider) = provider {
                connect_provider.set(Some(provider.clone()));
            }
            true
        }
        "logout" => {
            navigate.call(crate::routes::settings_pane_href("models"));
            true
        }
        "new" => {
            crate::components::agent_chat::launch_agent(
                pump,
                conversation.worker_fp.clone(),
                conversation.cwd.clone(),
                navigate,
            );
            true
        }
        "export" => {
            let title = safe_download_title(&conversation.title);
            let markdown = roost_client_core::client::agent_chat::export::transcript_to_markdown(
                &conversation.title,
                scope.transcript,
            );
            crate::platform::file_save::save_text_file(
                &format!("{title}.md"),
                "text/markdown",
                &markdown,
            );
            true
        }
        _ => false,
    }
}

fn safe_download_title(title: &str) -> String {
    let cleaned = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        "Conversation".to_owned()
    } else {
        cleaned
    }
}

pub(super) fn model_picker_trigger_id(conversation_id: &str) -> String {
    format!("agent-chat-model-trigger-{conversation_id}")
}

#[cfg(target_arch = "wasm32")]
pub(super) fn open_model_picker(conversation_id: &str) {
    use wasm_bindgen::JsCast;
    if let Some(button) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&model_picker_trigger_id(conversation_id)))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        button.click();
    }
}
