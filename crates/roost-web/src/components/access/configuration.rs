//! Authorized previews and explicit application of host-owned access changes.
//! The coordinator supplies ownership, capabilities and durable operation status.
//! A scheduled helper is not a verified route; this view never reloads or fails over.

use dioxus::prelude::*;
use roost_client_core::client::rpc::UnaryMethod;
use roost_client_core::client::rpc::calls::settings::access::{
    AccessMode, AccessOperationState, AccessOwner, ApplyAccess, GetAccessSettings, PreviewAccess,
};

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle, FieldError,
    Select, SelectOption, TextField,
};
use crate::pump::use_store;

type AccessPreview = <PreviewAccess as UnaryMethod>::Response;

#[component]
pub fn AccessConfiguration() -> Element {
    let pump = use_store();
    let mut operation_id = use_signal(|| None::<String>);
    let reading = pump.clone();
    let mut settings = use_resource(move || {
        let rpc = reading.rpc();
        let operation_id = operation_id();
        async move {
            rpc.call(&GetAccessSettings { operation_id })
                .await
                .map_err(|error| error.to_string())
        }
    });
    let mut mode = use_signal(|| "tailscale".to_owned());
    let mut candidate = use_signal(String::new);
    let mut preview = use_signal(|| None::<AccessPreview>);
    let mut busy = use_signal(|| false);
    let mut failure = use_signal(|| None::<String>);
    let preview_pump = pump.clone();
    let build_preview = use_callback(move |()| {
        if busy() {
            return;
        }
        let external = settings.peek().as_ref().is_some_and(|reply| {
            reply
                .as_ref()
                .is_ok_and(|config| config.owner.as_known() == Some(AccessOwner::Flux))
        });
        let selected_mode = if external {
            AccessMode::PublicHttps
        } else {
            match mode().as_str() {
                "public-https" => AccessMode::PublicHttps,
                _ => AccessMode::Tailscale,
            }
        };
        let candidate_origin = if selected_mode == AccessMode::PublicHttps {
            candidate().trim().to_owned()
        } else {
            String::new()
        };
        let request = PreviewAccess {
            mode: selected_mode,
            candidate_origin,
        };
        busy.set(true);
        failure.set(None);
        preview.set(None);
        let rpc = preview_pump.rpc();
        spawn(async move {
            match rpc.call(&request).await {
                Ok(proposal) => preview.set(Some(proposal)),
                Err(error) => failure.set(Some(error.to_string())),
            }
            busy.set(false);
        });
    });
    let apply_pump = pump.clone();
    let apply = use_callback(move |()| {
        if busy() {
            return;
        }
        let Some(plan_token) = preview
            .peek()
            .as_ref()
            .filter(|plan| !plan.proposal_only && !plan.plan_token.is_empty())
            .map(|plan| plan.plan_token.clone())
        else {
            return;
        };
        busy.set(true);
        failure.set(None);
        let rpc = apply_pump.rpc();
        spawn(async move {
            match rpc.call(&ApplyAccess { plan_token }).await {
                Ok(accepted) if accepted.accepted => {
                    operation_id.set(Some(accepted.operation_id));
                    preview.set(None);
                }
                Ok(_) => failure.set(Some(
                    "The coordinator did not schedule the change.".to_owned(),
                )),
                Err(error) => {
                    failure.set(Some(format!("The change was not confirmed: {error}. Refresh status before trying another change.")));
                    preview.set(None);
                    settings.restart();
                }
            }
            busy.set(false);
        });
    });
    let response = settings.read();
    let can_apply = response
        .as_ref()
        .is_some_and(|reply| reply.as_ref().is_ok_and(|config| config.can_apply));
    rsx! {
        div { class: "flex flex-col gap-5", "data-testid": "access-configuration",
            Card {
                CardHeader {
                    CardTitle { "Installation access" }
                    CardDescription { "Review changes on the machine or deployment that owns this Roost. Browser bookmarks do not configure its network." }
                }
                CardContent { class: "flex flex-col gap-3",
                    match response.as_ref() {
                        None => rsx! { p { class: "m-0 text-muted-foreground", "Reading configuration ownership…" } },
                        Some(Err(message)) => rsx! { FieldError { "Cannot read access configuration: {message}" } },
                        Some(Ok(config)) => {
                            let external = config.owner.as_known() == Some(AccessOwner::Flux);
                            let owner = if external { "Kubernetes / Flux" } else if config.owner.as_known() == Some(AccessOwner::LocalService) { "Local installed service" } else { "Not established" };
                            let verification = if config.owner_verified { "Observed ownership" } else { "Ownership has not been independently verified" };
                            rsx! {
                                dl { class: "grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-2",
                                    dt { class: "text-muted-foreground", "Configuration owner" }
                                    dd { class: "m-0", "{owner}" }
                                    dt { class: "text-muted-foreground", "Verification" }
                                    dd { class: "m-0", "{verification}" }
                                    dt { class: "text-muted-foreground", "Current browser origin" }
                                    dd { class: "m-0 break-all font-mono", if config.browser_origin.is_empty() { "Not declared; see the effective address above" } else { "{config.browser_origin}" } }
                                    dt { class: "text-muted-foreground", "Current worker origin" }
                                    dd { class: "m-0 break-all font-mono", "{config.worker_origin}" }
                                }
                                if !config.unavailable_reason.is_empty() {
                                    p { class: "m-0 text-sm text-muted-foreground", "{config.unavailable_reason}" }
                                }
                                if !config.operation_id.is_empty() {
                                    p { class: "m-0 text-sm", "Operation: {config.operation_id}" }
                                    p { class: "m-0 text-sm", "{operation_label(config.operation_state.as_known())}" }
                                    p { class: "m-0 text-sm text-muted-foreground", "{config.operation_detail}" }
                                }
                                if config.can_preview {
                                    if external {
                                        p { class: "m-0 text-sm text-muted-foreground", "Public HTTPS changes produce a reviewable Flux proposal. This browser will not modify the cluster or start a host coordinator." }
                                    } else {
                                        Select {
                                            label: "Access mode",
                                            value: mode(),
                                            options: vec![SelectOption::new("tailscale", "Private HTTPS — Tailscale"), SelectOption::new("public-https", "Public HTTPS — Cloudflare / custom proxy")],
                                            disabled: busy(),
                                            on_change: move |value| { mode.set(value); preview.set(None); },
                                        }
                                    }
                                    if !external && mode() == "tailscale" && !config.can_configure_tailscale {
                                        p { class: "m-0 text-sm text-muted-foreground", "{config.tailscale_unavailable_reason}" }
                                    }
                                    if external || mode() == "public-https" {
                                        TextField {
                                            label: Some("Stable browser HTTPS origin".to_owned()),
                                            placeholder: Some("https://roost.example.com".to_owned()),
                                            value: candidate(),
                                            disabled: busy(),
                                            on_input: move |value| { candidate.set(value); preview.set(None); },
                                        }
                                    }
                                    p { class: "m-0 text-sm text-muted-foreground", "The previous route must remain available until the replacement is verified. A new browser origin needs explicit pairing. Workers keep their separately configured origin." }
                                    div {
                                        Button { variant: ButtonVariant::Default, disabled: busy() || (!external && mode() == "tailscale" && !config.can_configure_tailscale), onclick: move |_| build_preview.call(()), if busy() { "Working…" } else { "Preview change" } }
                                    }
                                }
                            }
                        },
                    }
                    if let Some(message) = failure() { FieldError { {message} } }
                    div {
                        Button { variant: ButtonVariant::Secondary, onclick: move |_| settings.restart(), "Refresh status" }
                    }
                }
            }
            if let Some(proposal) = preview.read().as_ref() {
                PreviewReview {
                    review: proposal.review.clone(),
                    origin: proposal.candidate_origin.clone(),
                    worker_origin: proposal.worker_origin.clone(),
                    proposal_only: proposal.proposal_only,
                    can_apply: can_apply && !proposal.plan_token.is_empty(),
                    busy: busy(),
                    on_apply: move |_| apply.call(()),
                }
            }
        }
    }
}

fn operation_label(state: Option<AccessOperationState>) -> &'static str {
    match state {
        Some(AccessOperationState::Scheduled) => "Scheduled — not yet applied or verified",
        Some(AccessOperationState::Applying) => "Applying the approved change",
        Some(AccessOperationState::Succeeded) => "The helper completed the change",
        Some(AccessOperationState::Failed) => "The change failed; read the operation detail",
        Some(AccessOperationState::None) => "No active operation",
        _ => "Unrecognized operation state",
    }
}

#[component]
fn PreviewReview(
    review: String,
    origin: String,
    worker_origin: String,
    proposal_only: bool,
    can_apply: bool,
    busy: bool,
    on_apply: EventHandler<()>,
) -> Element {
    let (copy, copied) = use_copy_clipboard(None, None);
    rsx! {
        Card { "data-testid": "access-change-preview",
            CardHeader {
                CardTitle { if proposal_only { "Unapplied deployment proposal" } else { "Review this change" } }
                CardDescription { "Previewing has not published this address or changed its ownership." }
            }
            CardContent { class: "flex flex-col gap-3",
                p { class: "m-0 break-all font-mono", "Browser: {origin}" }
                p { class: "m-0 break-all font-mono", "Workers: {worker_origin}" }
                pre { class: "m-0 whitespace-pre-wrap break-all font-mono text-sm", "{review}" }
                p { class: "m-0 text-sm text-muted-foreground", "Trust the new domain and proxy before applying. Route verification proves that traffic reaches Roost, not that the proxy cannot inspect or relay it." }
                div { class: "flex flex-wrap gap-2",
                    Button { variant: ButtonVariant::Secondary, onclick: move |_| copy(&review), if copied() { "Copied" } else { "Copy review" } }
                    if !proposal_only && can_apply {
                        Button { variant: ButtonVariant::Default, disabled: busy, onclick: move |_| on_apply.call(()), "Apply reviewed change" }
                    }
                }
            }
        }
    }
}
