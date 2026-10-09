//! The OS hand-offs a sidebar row menu offers for its machine: macOS Screen
//! Sharing (`vnc://`) and Finder (`smb://`), Windows Remote Desktop (a
//! generated `.rdp` file) and its UNC share path. Roost serves none of these;
//! it hands the machine's reachable address to the operator's own OS client.
//! Ports `apps/web/src/lib/machineActions.ts`; the sidebar menus list these and
//! `platform::machine_handoff` performs the chosen [`MachineLaunch`].

use roost_protocol::wire::{Worker, WorkerOs};

/// Why a disabled machine action is disabled.
pub const NO_REACHABLE_ADDRESS_TOOLTIP: &str =
    "No reachable address yet — the machine's worker must heartbeat its live tailnet name first.";

/// One hand-off a machine menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineAction {
    /// macOS: the machine's SMB shares in Finder.
    OpenInFinder,
    /// macOS: the built-in Screen Sharing client.
    ScreenSharing,
    /// Windows: `\\host\` on the clipboard.
    CopyNetworkSharePath,
    /// Windows: a `.rdp` connection file for the built-in client.
    RemoteDesktop,
}

impl MachineAction {
    /// What the menu item says.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenInFinder => "Open in Finder",
            Self::ScreenSharing => "Screen sharing",
            Self::CopyNetworkSharePath => "Copy network share path",
            Self::RemoteDesktop => "Remote Desktop",
        }
    }

    /// The `data-testid` stem, v2's action id.
    pub const fn test_id(self) -> &'static str {
        match self {
            Self::OpenInFinder => "finder",
            Self::ScreenSharing => "screen-share",
            Self::CopyNetworkSharePath => "network-share",
            Self::RemoteDesktop => "remote-desktop",
        }
    }
}

/// Which row menu is asking. A folder row is a workspace bucket, so it offers
/// the desktop hand-off only; a session row also offers the file share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineMenuKind {
    Folder,
    Session,
}

/// The hand-offs a machine on `os` offers in `menu`, in menu order.
///
/// Linux offers none: it ships no screen-sharing or file-sharing server a
/// stock install runs, so any URL would usually point at nothing.
pub fn machine_actions(os: WorkerOs, menu: MachineMenuKind) -> &'static [MachineAction] {
    match (os, menu) {
        (WorkerOs::Darwin, MachineMenuKind::Folder) => &[MachineAction::ScreenSharing],
        (WorkerOs::Darwin, MachineMenuKind::Session) => {
            &[MachineAction::OpenInFinder, MachineAction::ScreenSharing]
        }
        // VNC on Windows needs a server the operator installed (TightVNC,
        // UltraVNC, RealVNC); Remote Desktop is the built-in one.
        (WorkerOs::Win32, MachineMenuKind::Folder) => {
            &[MachineAction::ScreenSharing, MachineAction::RemoteDesktop]
        }
        (WorkerOs::Win32, MachineMenuKind::Session) => &[
            MachineAction::CopyNetworkSharePath,
            MachineAction::ScreenSharing,
            MachineAction::RemoteDesktop,
        ],
        (WorkerOs::Linux, _) => &[],
    }
}

/// The address a hand-off may target: ONLY the worker's reported live address.
/// Never synthesized from the label — a label is a Tailscale HostName, which
/// does not resolve, and v2 shipped dead `vnc://worker-*` links that way.
pub fn reachable_host(worker: &Worker) -> Option<&str> {
    worker
        .reachable_addr
        .as_deref()
        .map(str::trim)
        .filter(|address| !address.is_empty())
}

/// An address that would not stay a bare host inside a URL, a UNC path or an
/// `.rdp` line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid machine address {0:?}")]
pub struct InvalidMachineAddress(pub String);

/// `host` trimmed, refused when empty or when it carries a character that
/// would change what the URL, UNC path or `.rdp` line means: a path separator,
/// a line break or other control, whitespace, or URL userinfo/query/fragment.
pub fn checked_host(host: &str) -> Result<&str, InvalidMachineAddress> {
    let host = host.trim();
    let unsafe_char = |character: char| {
        character.is_control()
            || character.is_whitespace()
            || matches!(character, '/' | '\\' | '@' | '?' | '#')
    };
    if host.is_empty() || host.chars().any(unsafe_char) {
        return Err(InvalidMachineAddress(host.to_owned()));
    }
    Ok(host)
}

/// The `.rdp` file the built-in Windows client opens: prompt for credentials,
/// require server authentication, share the clipboard. CRLF, as the client
/// writes it.
pub fn remote_desktop_file(host: &str) -> Result<String, InvalidMachineAddress> {
    let host = checked_host(host)?;
    Ok([
        format!("full address:s:{host}"),
        "prompt for credentials:i:1".to_owned(),
        "authentication level:i:2".to_owned(),
        "redirectclipboard:i:1".to_owned(),
        String::new(),
    ]
    .join("\r\n"))
}

/// The `.rdp` download's file name: the host with anything outside
/// `[A-Za-z0-9._-]` collapsed to `-`.
pub fn remote_desktop_file_name(host: &str) -> String {
    let mut name = String::with_capacity(host.len() + 4);
    let mut collapsing = false;
    for character in host.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            name.push(character);
            collapsing = false;
        } else if !collapsing {
            name.push('-');
            collapsing = true;
        }
    }
    name.push_str(".rdp");
    name
}

/// What performing an action asks of the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLaunch {
    /// Assign `location.href`, which hands a non-web scheme to the OS.
    Navigate { href: String },
    /// Write `text` to the clipboard.
    CopyText { text: String },
    /// Save `contents` as `file_name` with `mime_type`.
    Download {
        file_name: String,
        mime_type: &'static str,
        contents: String,
    },
}

/// The browser step for `action` against `host`.
pub fn machine_launch(
    action: MachineAction,
    host: &str,
) -> Result<MachineLaunch, InvalidMachineAddress> {
    let host = checked_host(host)?;
    Ok(match action {
        MachineAction::OpenInFinder => MachineLaunch::Navigate {
            href: format!("smb://{host}"),
        },
        MachineAction::ScreenSharing => MachineLaunch::Navigate {
            href: format!("vnc://{host}"),
        },
        MachineAction::CopyNetworkSharePath => MachineLaunch::CopyText {
            text: format!("\\\\{host}\\"),
        },
        MachineAction::RemoteDesktop => MachineLaunch::Download {
            file_name: remote_desktop_file_name(host),
            mime_type: "application/x-rdp",
            contents: remote_desktop_file(host)?,
        },
    })
}
