//! One pending pair request, with its provenance and its two answers.
//!
//! The raw user agent and the request id sit behind a per-card "Technical
//! details" disclosure, because both are evidence an approver checks and neither
//! is something to read before deciding.

use dioxus::prelude::*;
use roost_client_core::store::PairRequest;

use crate::components::terminal::dom::now_ms;

use libero::{
    components::{Badge, Button, Flex, Paper, Pictogram, Text, Title},
    sx::sx,
};

use super::approver::PairApprover;

/// The card for one request. Expired requests render nothing at all: a request
/// the coordinator will no longer accept is not a decision anybody can make, and
/// offering "Approve" on one is a control that cannot work.
#[component]
pub fn PairRequestCard(request: PairRequest, busy: bool, approver: PairApprover) -> Element {
    let mut details_open = use_signal(|| false);
    let details_id = libero::hooks::use_id().read().clone();
    let card_sx = use_hook(|| sx().padding("md").width("100%"));
    let rows_sx = use_hook(|| sx().gap("sm"));
    let row_sx = use_hook(|| sx().gap("sm").align_items("start"));
    let code_sx = use_hook(|| sx().overflow_wrap("anywhere"));
    let identity_color = match identity_status(&request) {
        "ok" => "success",
        "warn" => "warning",
        _ => "muted",
    };
    let now = now_ms() as i64;
    if !request.is_live_at(now) {
        return rsx! {};
    }
    let ephemeral_id = request.ephemeral_id.clone();
    let label = request.label.clone();
    let expires_at_ms = request.expires_at_ms;
    let device_type = request.client_device_type.trim().to_string();
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
        Paper { sx: card_sx, shadow: "sm", "data-testid": "pair-request-card", "data-ephemeral-id": ephemeral_id.clone(),
            Flex { direction: "column", gap: "md",
                Flex { direction: "column", gap: "xs",
                    Title { size: "md", "New browser wants to pair" }
                    Text { size: "sm", color: "muted",
                        "Review the browser and network details before allowing access."
                    }
                }
                Flex { direction: "column", gap: "sm",
                    Flex { sx: row_sx.clone(), "data-testid": "pair-request-device",
                        Pictogram { icon: pictogram_icons_lucide::monitor_smartphone::outlined, width: "16", height: "16" }
                        Flex { direction: "row", wrap: true, gap: "xs",
                            Text { size: "sm", "{label}" }
                            if !device_type.is_empty() { Badge { variant: "outlined", "{device_type}" } }
                        }
                    }
                    Flex { sx: row_sx.clone(), "data-testid": "pair-request-location",
                        Pictogram { icon: pictogram_icons_lucide::map_pin::outlined, width: "16", height: "16" }
                        Text { size: "sm", "{location_label(&request)}" }
                    }
                    Flex { sx: row_sx.clone(), "data-testid": "pair-request-network",
                        Pictogram { icon: pictogram_icons_lucide::network::outlined, width: "16", height: "16" }
                        Text { size: "sm", "{network_label(&request)}" }
                    }
                    Flex { sx: row_sx.clone(), "data-testid": "pair-request-identity",
                        Pictogram { icon: pictogram_icons_lucide::shield_check::outlined, width: "16", height: "16" }
                        Text { size: "sm", "{identity_label(&request)}" }
                        Badge {
                            color: identity_color,
                            variant: "tonal",
                            match identity_status(&request) {
                                "ok" => "Verified",
                                "warn" => "Unverified",
                                _ => "Unavailable",
                            }
                        }
                    }
                    Flex { sx: row_sx.clone(), "data-testid": "pair-request-expiry",
                        Pictogram { icon: pictogram_icons_lucide::clock::outlined, width: "16", height: "16" }
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", "{expiry_label(request.expires_at_ms, now)}" }
                            Text { size: "xs", color: "muted", "{relative_age(request.created_at_ms, now)}" }
                        }
                    }
                }
                Button {
                    variant: "text",
                    icon: rsx! { Pictogram { icon: if details_expanded { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined }, width: "16", height: "16" } },
                    "aria-expanded": details_expanded.to_string(),
                    "aria-controls": details_id.clone(),
                    "data-testid": "pair-request-technical-details-toggle",
                    onclick: move |_| details_open.toggle(),
                    "Technical details"
                }
                if details_expanded {
                    Paper { id: details_id.clone(), sx: rows_sx,
                        Flex { direction: "column", gap: "sm",
                            Flex { sx: row_sx.clone(), "data-testid": "pair-request-user-agent",
                                Pictogram { icon: pictogram_icons_lucide::languages::outlined, width: "16", height: "16" }
                                Flex { direction: "column", gap: "xs",
                                    Text { component: "strong", size: "sm", "User agent" }
                                    Text { component: "code", size: "xs", sx: code_sx.clone(), "{user_agent_label(&request)}" }
                                }
                            }
                            Flex { sx: row_sx, "data-testid": "pair-request-id",
                                Pictogram { icon: pictogram_icons_lucide::key_round::outlined, width: "16", height: "16" }
                                Flex { direction: "column", gap: "xs",
                                    Text { component: "strong", size: "sm", "Request ID" }
                                    Text { component: "code", size: "xs", sx: code_sx, "{request.ephemeral_id}" }
                                }
                            }
                        }
                    }
                }
                Flex { direction: "row", gap: "sm", wrap: true, role: "group", "aria-label": "Pair request actions",
                    Button {
                        variant: "outlined",
                        icon: rsx! { Pictogram { icon: pictogram_icons_lucide::x::outlined, width: "16", height: "16" } },
                        "data-testid": "pair-card-dismiss",
                        disabled: busy,
                        onclick: on_deny,
                        "Deny"
                    }
                    Button {
                        variant: "filled",
                        icon: rsx! { Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "16", height: "16" } },
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
