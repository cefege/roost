//! One pending pair request, with its provenance and its two answers.
//!
//! The raw user agent and the request id sit behind a per-card "Technical
//! details" disclosure, because both are evidence an approver checks and neither
//! is something to read before deciding.

use dioxus::prelude::*;
use roost_client_core::store::PairRequest;

use crate::components::terminal::dom::now_ms;
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardContent, CardDescription, CardFooter, CardHeader,
    CardTitle, CardVariant, Chip, IconType, List, ListRow, StatusDot,
};

use super::approver::PairApprover;

/// The card for one request. Expired requests render nothing at all: a request
/// the coordinator will no longer accept is not a decision anybody can make, and
/// offering "Approve" on one is a control that cannot work.
#[component]
pub fn PairRequestCard(request: PairRequest, busy: bool, approver: PairApprover) -> Element {
    let mut details_open = use_signal(|| false);
    let details_id = use_hook(|| scoped_element_id("pair-request-details"));
    let now = now_ms() as i64;
    if !request.is_live_at(now) {
        return rsx! {};
    }
    let ephemeral_id = request.ephemeral_id.clone();
    let label = request.label.clone();
    let expires_at_ms = request.expires_at_ms;
    let device_type = request.client_device_type.trim().to_string();
    // Each answer is built once: an `rsx!` attribute body is a block, and a
    // block that clones per attribute moves the same `approver` once per
    // attribute it appears in.
    let on_deny = {
        let approver = approver.clone();
        let ephemeral_id = ephemeral_id.clone();
        move |_event: MouseEvent| approver.deny(&ephemeral_id)
    };
    let on_approve = {
        let approver = approver.clone();
        let ephemeral_id = ephemeral_id.clone();
        let label = label.clone();
        move |_event: MouseEvent| approver.approve(&ephemeral_id, &label, expires_at_ms)
    };
    let details_expanded = *details_open.read();
    rsx! {
        div { "data-testid": "pair-request-card", "data-ephemeral-id": ephemeral_id.clone(),
            Card { variant: CardVariant::Elevated,
                CardHeader {
                    CardTitle { "New browser wants to pair" }
                    CardDescription {
                        "Review the browser and network details before allowing access."
                    }
                }
                CardContent {
                    List { contained: true,
                        ListRow {
                            leading_icon: Some(IconType::MonitorSmartphone),
                            test_id: Some("pair-request-device".to_owned()),
                            headline: rsx! { span { class: "text-sm", {label.clone()} } },
                            trailing: if device_type.is_empty() {
                                None
                            } else {
                                Some(rsx! { Chip { label: device_type.clone() } })
                            },
                        }
                        ListRow {
                            leading_icon: Some(IconType::MapPin),
                            test_id: Some("pair-request-location".to_owned()),
                            headline: rsx! { span { class: "text-sm", {location_label(&request)} } },
                        }
                        ListRow {
                            leading_icon: Some(IconType::Network),
                            test_id: Some("pair-request-network".to_owned()),
                            headline: rsx! { span { class: "text-sm", {network_label(&request)} } },
                        }
                        ListRow {
                            leading_icon: Some(IconType::ShieldCheck),
                            test_id: Some("pair-request-identity".to_owned()),
                            headline: rsx! {
                                span { class: "inline-flex items-center gap-2 text-sm",
                                    StatusDot { status: identity_status(&request).to_owned() }
                                    span { {identity_label(&request)} }
                                }
                            },
                        }
                        ListRow {
                            leading_icon: Some(IconType::Clock),
                            test_id: Some("pair-request-expiry".to_owned()),
                            headline: rsx! {
                                span { class: "text-sm", {expiry_label(request.expires_at_ms, now)} }
                            },
                            support: Some(rsx! {
                                span { class: "text-xs text-muted-foreground", {relative_age(request.created_at_ms, now)} }
                            }),
                        }
                    }
                    div { class: "mt-3",
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            icon: if details_expanded { Some(IconType::ChevronUp) } else { Some(IconType::ChevronDown) },
                            "aria-expanded": details_expanded.to_string(),
                            "aria-controls": details_id.clone(),
                            "data-testid": "pair-request-technical-details-toggle",
                            onclick: move |_| details_open.toggle(),
                            "Technical details"
                        }
                    }
                    if details_expanded {
                        div { id: details_id.clone(), class: "mt-3",
                            List { contained: true,
                                ListRow {
                                    leading_icon: Some(IconType::Languages),
                                    test_id: Some("pair-request-user-agent".to_owned()),
                                    headline: rsx! { span { class: "text-sm font-medium", "User agent" } },
                                    support: Some(rsx! {
                                        span { class: "text-xs break-all", {user_agent_label(&request)} }
                                    }),
                                }
                                ListRow {
                                    leading_icon: Some(IconType::KeyRound),
                                    test_id: Some("pair-request-id".to_owned()),
                                    headline: rsx! { span { class: "text-sm font-medium", "Request ID" } },
                                    support: Some(rsx! {
                                        code { class: "text-xs break-all", {request.ephemeral_id.clone()} }
                                    }),
                                }
                            }
                        }
                    }
                }
                CardFooter {
                    div { class: "flex flex-wrap gap-2", role: "group", "aria-label": "Pair request actions",
                        Button {
                            variant: ButtonVariant::Outline,
                            icon: Some(IconType::X),
                            "data-testid": "pair-card-dismiss",
                            disabled: busy,
                            onclick: on_deny,
                            "Deny"
                        }
                        Button {
                            icon: Some(IconType::Check),
                            "data-testid": "pair-card-approve",
                            disabled: busy,
                            onclick: on_approve,
                            if busy { "Approving…" } else { "Approve" }
                        }
                    }
                }
            }
        }
    }
}

/// Where the request arrived from, in the reader's words, or an honest absence.
pub fn location_label(request: &PairRequest) -> String {
    let joined = [
        request.city.as_str(),
        request.region.as_str(),
        request.country_code.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .collect::<Vec<&str>>()
    .join(", ");
    if joined.is_empty() {
        "Location unavailable".to_string()
    } else {
        joined
    }
}

/// The edge's address for the request, or an honest absence.
pub fn network_label(request: &PairRequest) -> String {
    let source = request.source_ip.trim();
    if source.is_empty() {
        "Network unavailable".to_string()
    } else {
        source.to_string()
    }
}

/// What the front door could vouch for, or that it could not.
pub fn identity_label(request: &PairRequest) -> String {
    let identity = request.edge_identity.trim();
    if identity.is_empty() {
        "No front-door identity".to_string()
    } else if request.edge_identity_verified {
        format!("Signed in as {identity}")
    } else {
        format!("Claimed identity {identity}")
    }
}

/// The identity row's dot, which says whether the claim was verified.
pub fn identity_status(request: &PairRequest) -> &'static str {
    let identity = request.edge_identity.trim();
    if identity.is_empty() {
        "idle"
    } else if request.edge_identity_verified {
        "ok"
    } else {
        "warn"
    }
}

/// When the request stops being answerable, in whole minutes.
pub fn expiry_label(expires_at_ms: i64, now: i64) -> String {
    if expires_at_ms <= 0 {
        return "Expiry unavailable".to_string();
    }
    let minutes = expires_at_ms.saturating_sub(now).max(0).div_euclid(60_000);
    format!("Expires in {minutes}m")
}

/// How long ago the request arrived.
pub fn relative_age(created_at_ms: i64, now: i64) -> String {
    if created_at_ms <= 0 {
        return "Request age unavailable".to_string();
    }
    let age_ms = now.saturating_sub(created_at_ms).max(0);
    if age_ms < 60_000 {
        "Requested just now".to_string()
    } else if age_ms < 3_600_000 {
        format!("Requested {}m ago", age_ms / 60_000)
    } else if age_ms < 86_400_000 {
        format!("Requested {}h ago", age_ms / 3_600_000)
    } else {
        format!("Requested {}d ago", age_ms / 86_400_000)
    }
}

/// The user agent, or an honest absence.
pub fn user_agent_label(request: &PairRequest) -> String {
    let agent = request.user_agent.trim();
    if agent.is_empty() {
        "User agent unavailable".to_string()
    } else {
        agent.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PairRequest {
        PairRequest {
            ephemeral_id: "e".repeat(32),
            label: "Firefox on Linux".to_string(),
            created_at_ms: 1_700_000_000_000,
            user_agent: "Mozilla/5.0".to_string(),
            client_browser: "Firefox".to_string(),
            client_os: "Linux".to_string(),
            client_device_type: "Desktop".to_string(),
            source_ip: "203.0.113.4".to_string(),
            country_code: "NL".to_string(),
            region: "NH".to_string(),
            city: "Amsterdam".to_string(),
            edge_identity_provider: String::new(),
            edge_identity: String::new(),
            edge_identity_verified: false,
            expires_at_ms: 1_700_003_600_000,
        }
    }

    #[test]
    fn a_row_with_no_edge_evidence_says_so_rather_than_rendering_nothing() {
        let row = request();
        assert_eq!(location_label(&row), "Amsterdam, NH, NL");
        assert_eq!(network_label(&row), "203.0.113.4");
        assert_eq!(identity_label(&row), "No front-door identity");
        assert_eq!(identity_status(&row), "idle");
    }

    #[test]
    fn a_claimed_identity_is_not_reported_as_a_verified_one() {
        let mut row = request();
        row.edge_identity = "ana@example.com".to_string();
        assert_eq!(identity_status(&row), "warn");
        assert_eq!(identity_label(&row), "Claimed identity ana@example.com");
        row.edge_identity_verified = true;
        assert_eq!(identity_status(&row), "ok");
        assert_eq!(identity_label(&row), "Signed in as ana@example.com");
    }

    #[test]
    fn expiry_counts_whole_minutes_and_never_goes_negative() {
        assert_eq!(
            expiry_label(1_700_003_600_000, 1_700_003_600_000),
            "Expires in 0m"
        );
        assert_eq!(
            expiry_label(1_700_003_659_000, 1_700_003_600_000),
            "Expires in 0m"
        );
        assert_eq!(
            expiry_label(1_700_003_720_000, 1_700_003_600_000),
            "Expires in 2m"
        );
        assert_eq!(
            expiry_label(1_700_003_540_000, 1_700_003_600_000),
            "Expires in 0m"
        );
        assert_eq!(expiry_label(0, 1), "Expiry unavailable");
    }

    #[test]
    fn age_is_read_in_the_units_a_person_thinks_in() {
        let now = 1_700_000_000_000;
        assert_eq!(relative_age(0, now), "Request age unavailable");
        assert_eq!(relative_age(now, now), "Requested just now");
        assert_eq!(relative_age(now - 120_000, now), "Requested 2m ago");
        assert_eq!(relative_age(now - 7_200_000, now), "Requested 2h ago");
        assert_eq!(relative_age(now - 172_800_000, now), "Requested 2d ago");
    }

    #[test]
    fn a_request_stops_being_answerable_the_instant_it_expires() {
        let row = request();
        assert!(row.is_live_at(row.expires_at_ms - 1));
        assert!(!row.is_live_at(row.expires_at_ms));
    }
}
