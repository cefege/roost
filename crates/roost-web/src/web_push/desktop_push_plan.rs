//! The Desktop notifications decisions with no browser in them: what the
//! Settings switch shows, what a page load repairs, how a failure reads, and
//! where a notification click goes.
//!
//! Called by the Settings notifications pane, `push_lifecycle` and
//! `DesktopPushBridge`; the browser facts arrive as plain values from
//! `browser_push`, which is what lets every branch here run without one.

use roost_client_core::client::rpc::calls::settings::push::PushKeyError;
use roost_protocol::wire::SessionId;

use super::NAVIGATE_MESSAGE_TYPE;

/// `Notification.permission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPermission {
    /// Never asked, or the prompt was dismissed.
    Default,
    /// Notifications are allowed.
    Granted,
    /// Notifications are blocked until the reader changes site settings.
    Denied,
}

impl PushPermission {
    /// The browser's string. Anything unrecognised reads as `Default`: a value
    /// this build does not know is not a grant.
    #[must_use]
    pub fn from_browser(value: &str) -> Self {
        match value {
            "granted" => Self::Granted,
            "denied" => Self::Denied,
            _ => Self::Default,
        }
    }
}

/// Everything the Desktop row is drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPushFacts {
    /// The browser has a service worker container, `PushManager` and
    /// `Notification`. False in an iOS Safari tab that is not a Home Screen app.
    pub browser_supported: bool,
    /// The coordinator answered `PushGetConfig` with a key.
    pub coordinator_available: bool,
    /// The browser's notification permission.
    pub permission: PushPermission,
    /// The stored Desktop preference.
    pub enabled: bool,
    /// A subscribe or unsubscribe is in flight.
    pub busy: bool,
}

/// The Desktop row as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPushRow {
    /// Whether the switch shows on.
    pub checked: bool,
    /// Whether the switch refuses a click.
    pub disabled: bool,
    /// The support line.
    pub support: &'static str,
}

/// The Desktop row for `facts`.
///
/// The switch shows ON only when the preference AND the grant agree, because a
/// stored `true` the browser no longer permits delivers nothing.
#[must_use]
pub fn desktop_push_row(facts: DesktopPushFacts) -> DesktopPushRow {
    let checked = facts.enabled && facts.permission == PushPermission::Granted;
    let support = if !facts.browser_supported {
        "This browser cannot receive push notifications. On iPhone and iPad, add Roost to the Home Screen and open it from there first."
    } else if !facts.coordinator_available {
        "Push delivery is off on this coordinator: its operator has not allowed any push service."
    } else if facts.permission == PushPermission::Denied {
        "Notifications are blocked. Allow Roost in this browser's site settings, then try again."
    } else if facts.busy {
        "Updating this browser's notification subscription…"
    } else if checked {
        "Enabled for this browser. A device already viewing the terminal is not notified."
    } else {
        "Get an OS notification even when Roost is closed. Enabling asks for browser permission."
    };
    let disabled = facts.busy
        || !facts.browser_supported
        || !facts.coordinator_available
        || (facts.permission == PushPermission::Denied && !checked);
    DesktopPushRow {
        checked,
        disabled,
        support,
    }
}

/// What a page load does to bring the browser, the coordinator and the
/// preference back into agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushReconcile {
    /// They agree.
    Settled,
    /// Send the browser's subscription again: a push service rotates
    /// endpoints, and the coordinator prunes one that answered 404 or 410.
    Refresh,
    /// The grant stands but the subscription is gone (site data cleared):
    /// subscribe again. No prompt, because permission is already granted.
    Resubscribe,
    /// The preference claims delivery the browser no longer permits: drop the
    /// subscription and turn the preference off.
    Revoke,
    /// A subscription outlived its preference: drop it on both sides.
    DropStale,
}

/// The repair a page load owes. A coordinator with push off is left alone, so
/// an operator re-enabling it finds every enabled browser still enabled.
#[must_use]
pub fn reconcile_desktop_push(
    enabled: bool,
    permission: PushPermission,
    has_subscription: bool,
    coordinator_available: bool,
) -> PushReconcile {
    if !coordinator_available {
        return PushReconcile::Settled;
    }
    match (
        enabled,
        permission == PushPermission::Granted,
        has_subscription,
    ) {
        (true, true, true) => PushReconcile::Refresh,
        (true, true, false) => PushReconcile::Resubscribe,
        (true, false, _) => PushReconcile::Revoke,
        (false, _, true) => PushReconcile::DropStale,
        (false, _, false) => PushReconcile::Settled,
    }
}

/// Why turning Desktop notifications on or off did not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DesktopPushError {
    /// No service worker container, `PushManager` or `Notification`.
    #[error(
        "This browser cannot receive push notifications. On iPhone and iPad, add Roost to the Home Screen first."
    )]
    Unsupported,
    /// The reader blocked notifications.
    #[error(
        "Notifications are blocked. Allow Roost in this browser's site settings, then try again."
    )]
    PermissionDenied,
    /// The reader closed the prompt without answering.
    #[error("Notification permission was not granted.")]
    PermissionDismissed,
    /// The coordinator's key is missing or unusable.
    #[error("Push notifications are not available on this coordinator: {0}")]
    Key(PushKeyError),
    /// The browser refused a step, in its own words.
    #[error("The browser refused: {0}")]
    Browser(String),
    /// The coordinator refused a call, in its own words.
    #[error("The coordinator refused: {0}")]
    Coordinator(String),
}

impl From<PushKeyError> for DesktopPushError {
    fn from(error: PushKeyError) -> Self {
        Self::Key(error)
    }
}

/// The outcome of the permission prompt, as the enable path needs it.
pub fn require_granted(permission: PushPermission) -> Result<(), DesktopPushError> {
    match permission {
        PushPermission::Granted => Ok(()),
        PushPermission::Denied => Err(DesktopPushError::PermissionDenied),
        PushPermission::Default => Err(DesktopPushError::PermissionDismissed),
    }
}

/// The preference after a toggle settles: ON only when the reader asked for it
/// and every step succeeded; OFF whenever they asked for off, because the local
/// unsubscribe already stopped delivery even if the coordinator call failed.
#[must_use]
pub fn preference_after_toggle(requested: bool, outcome: &Result<(), DesktopPushError>) -> bool {
    requested && outcome.is_ok()
}

/// Where a notification click asks this window to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationClick {
    /// A session's terminal.
    Session(SessionId),
    /// The pairing approvals at /pair.
    PairApprovals,
}

/// The destination a service-worker `message` names, when it is a
/// notification click: the pairing approvals, or a well-formed session id.
#[must_use]
pub fn clicked_target(
    message_type: Option<&str>,
    session_id: Option<&str>,
    target: Option<&str>,
) -> Option<NotificationClick> {
    if message_type != Some(NAVIGATE_MESSAGE_TYPE) {
        return None;
    }
    if target == Some("pair") {
        return Some(NotificationClick::PairApprovals);
    }
    SessionId::try_from(session_id?)
        .ok()
        .map(NotificationClick::Session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> DesktopPushFacts {
        DesktopPushFacts {
            browser_supported: true,
            coordinator_available: true,
            permission: PushPermission::Default,
            enabled: false,
            busy: false,
        }
    }

    #[test]
    fn the_switch_is_on_only_when_the_preference_and_the_grant_agree() {
        let granted = DesktopPushFacts {
            enabled: true,
            permission: PushPermission::Granted,
            ..facts()
        };
        assert!(desktop_push_row(granted).checked);
        assert!(!desktop_push_row(granted).disabled);
        let revoked = DesktopPushFacts {
            permission: PushPermission::Default,
            ..granted
        };
        assert!(!desktop_push_row(revoked).checked);
        let off = DesktopPushFacts {
            enabled: false,
            ..granted
        };
        assert!(!desktop_push_row(off).checked);
    }

    #[test]
    fn a_switch_that_cannot_deliver_refuses_the_click_and_says_why() {
        let unsupported = desktop_push_row(DesktopPushFacts {
            browser_supported: false,
            ..facts()
        });
        assert!(unsupported.disabled);
        assert!(unsupported.support.contains("Home Screen"));
        let off = desktop_push_row(DesktopPushFacts {
            coordinator_available: false,
            ..facts()
        });
        assert!(off.disabled);
        assert!(off.support.contains("coordinator"));
        let denied = desktop_push_row(DesktopPushFacts {
            permission: PushPermission::Denied,
            ..facts()
        });
        assert!(denied.disabled);
        assert!(denied.support.contains("blocked"));
        let busy = desktop_push_row(DesktopPushFacts {
            busy: true,
            ..facts()
        });
        assert!(busy.disabled);
        assert!(!desktop_push_row(facts()).disabled);
    }

    #[test]
    fn a_page_load_repairs_each_disagreement_one_way() {
        use PushPermission::{Default, Denied, Granted};
        use PushReconcile::{DropStale, Refresh, Resubscribe, Revoke, Settled};
        assert_eq!(reconcile_desktop_push(true, Granted, true, true), Refresh);
        assert_eq!(
            reconcile_desktop_push(true, Granted, false, true),
            Resubscribe
        );
        assert_eq!(reconcile_desktop_push(true, Denied, true, true), Revoke);
        assert_eq!(reconcile_desktop_push(true, Default, false, true), Revoke);
        assert_eq!(
            reconcile_desktop_push(false, Granted, true, true),
            DropStale
        );
        assert_eq!(reconcile_desktop_push(false, Denied, true, true), DropStale);
        assert_eq!(reconcile_desktop_push(false, Granted, false, true), Settled);
    }

    #[test]
    fn a_coordinator_with_push_off_leaves_every_browser_as_it_was() {
        for enabled in [true, false] {
            for has_subscription in [true, false] {
                assert_eq!(
                    reconcile_desktop_push(
                        enabled,
                        PushPermission::Denied,
                        has_subscription,
                        false
                    ),
                    PushReconcile::Settled
                );
            }
        }
    }

    #[test]
    fn only_a_granted_prompt_continues_the_enable() {
        assert_eq!(require_granted(PushPermission::Granted), Ok(()));
        assert_eq!(
            require_granted(PushPermission::Denied),
            Err(DesktopPushError::PermissionDenied)
        );
        assert_eq!(
            require_granted(PushPermission::Default),
            Err(DesktopPushError::PermissionDismissed)
        );
        assert_eq!(
            PushPermission::from_browser("granted"),
            PushPermission::Granted
        );
        assert_eq!(
            PushPermission::from_browser("denied"),
            PushPermission::Denied
        );
        assert_eq!(
            PushPermission::from_browser("prompt"),
            PushPermission::Default
        );
    }

    #[test]
    fn the_preference_follows_the_request_only_when_it_completed() {
        let refused = Err(DesktopPushError::Coordinator("full".to_owned()));
        assert!(preference_after_toggle(true, &Ok(())));
        assert!(!preference_after_toggle(true, &refused));
        assert!(!preference_after_toggle(false, &Ok(())));
        assert!(!preference_after_toggle(false, &refused));
    }

    #[test]
    fn a_click_message_names_a_session_only_when_well_formed() {
        let id = "11111111-1111-4111-8111-111111111111";
        assert_eq!(
            clicked_target(Some(NAVIGATE_MESSAGE_TYPE), Some(id), None),
            SessionId::try_from(id).ok().map(NotificationClick::Session)
        );
        assert_eq!(clicked_target(Some("other"), Some(id), None), None);
        assert_eq!(
            clicked_target(Some(NAVIGATE_MESSAGE_TYPE), Some("../settings"), None),
            None
        );
        assert_eq!(
            clicked_target(Some(NAVIGATE_MESSAGE_TYPE), None, None),
            None
        );
        assert_eq!(clicked_target(None, Some(id), None), None);
    }

    #[test]
    fn a_pairing_click_opens_the_approvals() {
        assert_eq!(
            clicked_target(Some(NAVIGATE_MESSAGE_TYPE), None, Some("pair")),
            Some(NotificationClick::PairApprovals)
        );
        assert_eq!(clicked_target(Some("other"), None, Some("pair")), None);
    }
}
