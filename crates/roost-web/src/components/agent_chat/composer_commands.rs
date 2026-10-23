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
    submit_message(
        pump,
        conversation_id,
        command,
        "steer".to_string(),
        super::request_ids::new_request_id(),
        outcome,
    );
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
    when_busy: String,
    request_id: String,
    outcome: EventHandler<Result<(), String>>,
) {
    let label = text.clone();
    // The call and its report run apart. The call is unscoped, so it always
    // completes. The report is scoped to the component that sent it: the
    // composer can unmount while the call is in flight (its tab closes), and
    // calling a dropped component's `outcome` would panic the whole app.
    let (sent, answer) = futures::channel::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let result = match pump
            .rpc()
            .call(&SubmitAgentChat {
                conversation_id,
                text,
                when_busy,
                request_id,
                ..Default::default()
            })
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                tracing::warn!(target: "agent_chat", %error, "agent message submit failed");
                Err(format!("Could not run {label}: {error}"))
            }
        };
        let _ = sent.send(result);
    });
    spawn(async move {
        if let Ok(result) = answer.await {
            outcome.call(result);
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn submit_message(
    _: Pump,
    _: String,
    _: String,
    _: String,
    _: String,
    _: EventHandler<Result<(), String>>,
) {
}

pub(super) struct LocalCommandScope<'a> {
    pub(super) conversation: &'a ConversationSummary,
    pub(super) transcript: &'a Transcript,
    pub(super) catalog: Option<&'a ModelsCatalog>,
    pub(super) pump: Pump,
    pub(super) navigate: EventHandler<String>,
    pub(super) connect_provider: Signal<Option<ProviderEntry>>,
    pub(super) model_hub_open: Signal<bool>,
    pub(super) report: EventHandler<Result<(), String>>,
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
    let report = scope.report;
    match name {
        "model" if client_bare && args.is_empty() => {
            let mut open = scope.model_hub_open;
            open.set(true);
            true
        }
        "switch" if client_bare && args.is_empty() => {
            #[cfg(target_arch = "wasm32")]
            open_model_picker(&conversation.id);
            true
        }
        "settings" | "new" | "export" | "logout" if client_bare && !args.is_empty() => {
            report.call(Err(format!("Usage: /{name}")));
            true
        }
        "settings" if client_bare => {
            navigate.call(Route::Settings { pane: None }.to_path());
            true
        }
        "login" => {
            let providers = catalog.map_or(&[][..], |catalog| catalog.providers.as_slice());
            let provider = if args.is_empty() {
                providers.first()
            } else {
                providers.iter().find(|provider| provider.id == args)
            };
            match provider {
                Some(provider) => connect_provider.set(Some(provider.clone())),
                None if args.is_empty() => {}
                None => {
                    let known = providers
                        .iter()
                        .map(|provider| provider.id.as_str())
                        .collect::<Vec<_>>();
                    let known = if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    };
                    report.call(Err(format!(
                        "Unknown provider `{args}`. Known providers: {known}."
                    )));
                }
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
