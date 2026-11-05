//! Allowed native host-service effects and value admission.
//! The private coordinator router uses this before creating a pending effect.
//! Stats origins share the protocol's local-dashboard URL parser.

use roost_protocol::native_dashboard::parse_dashboard_origin;
#[derive(Clone, Copy)]
pub(super) enum HostServiceKind {
    CopyText,
    OpenUrl,
    OpenStatsDashboard,
}

impl HostServiceKind {
    pub(super) fn parse(kind: &str) -> Option<Self> {
        match kind {
            "copy_text" => Some(Self::CopyText),
            "open_url" => Some(Self::OpenUrl),
            "open_stats_dashboard" => Some(Self::OpenStatsDashboard),
            _ => None,
        }
    }
    pub(super) fn wire(&self) -> &'static str {
        match self {
            Self::CopyText => "copy_text",
            Self::OpenUrl => "open_url",
            Self::OpenStatsDashboard => "open_stats_dashboard",
        }
    }
    pub(super) fn value_is_safe(&self, value: &str) -> bool {
        if value.is_empty() || value.bytes().any(|byte| byte == 0) {
            return false;
        }
        match self {
            Self::CopyText => true,
            Self::OpenUrl => {
                if value.chars().any(char::is_control) {
                    return false;
                }
                // A path with no scheme is resolved by the browser's
                // authenticated file viewer; a scheme must be one this build
                // will hand to the platform opener.
                let Some(colon) = value.find(':') else {
                    return true;
                };
                let slash = value.find(['/', '\\']).unwrap_or(value.len());
                if colon > slash {
                    return true;
                }
                let scheme = value[..colon].to_ascii_lowercase();
                scheme.len() == 1 || matches!(scheme.as_str(), "http" | "https" | "mailto" | "file")
            }
            Self::OpenStatsDashboard => parse_dashboard_origin(value).is_some(),
        }
    }
}
