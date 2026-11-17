//! Service-manager and file ownership checks for the Tailscale proxy.
//! No shell or caller-selected executable is used.
//!
//! Coordinates the owned service definition and its listener verification.

#[path = "tailscale_proxy_service/probe.rs"]
mod probe;

use probe::{check_listener_pid, command, request};

use super::{
    HostPlatform, Receipt, TAILSCALE_PROXY_PORT, TailscaleError, TailscaleProxyController,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

const UNIT: &str = "roost-tailscale-proxy.service";
const LABEL: &str = "com.roost.tailscale-proxy";
const CADDY_PATHS: &[&str] = &[
    "/usr/bin/caddy",
    "/usr/local/bin/caddy",
    "/opt/homebrew/bin/caddy",
];

pub(super) fn allowed_caddy(path: &str) -> bool {
    CADDY_PATHS.contains(&path)
}
pub(super) fn find_caddy() -> Result<String, TailscaleError> {
    CADDY_PATHS
        .iter()
        .find(|path| Path::new(path).is_file())
        .map(|path| (*path).to_owned())
        .ok_or(TailscaleError::CaddyUnavailable)
}

pub(super) fn check_prerequisites(caddy: &str) -> Result<(), TailscaleError> {
    if !allowed_caddy(caddy) || !Path::new(caddy).is_file() {
        return Err(TailscaleError::CaddyUnavailable);
    }
    command(caddy, &["version"]).map_err(|_| TailscaleError::CaddyUnavailable)?;
    command("lsof", &["-v"]).map_err(|_| TailscaleError::ListenerInspectorUnavailable)?;
    Ok(())
}

pub(super) fn random_token() -> Result<String, TailscaleError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| TailscaleError::ReceiptIo)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn check_directory(path: &Path) -> Result<(), TailscaleError> {
    super::super::tailscale::tailscale_receipt::Store::snapshot(path).map(|_| ())
}

fn definition_path(controller: &TailscaleProxyController) -> Result<PathBuf, TailscaleError> {
    if !controller.home.is_absolute() || !controller.state.is_absolute() {
        return Err(TailscaleError::InsecureStateDirectory);
    }
    match controller.platform {
        HostPlatform::Linux => Ok(controller.home.join(".config/systemd/user").join(UNIT)),
        HostPlatform::MacOs => Ok(controller
            .home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))),
        HostPlatform::Windows => Err(TailscaleError::FilteringProxyRequired),
    }
}

fn text_path(path: &Path) -> Result<&str, TailscaleError> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or(TailscaleError::InsecureStateDirectory)
}
fn quote_systemd(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn configuration(receipt: &Receipt) -> String {
    format!(
        "{{\n admin off\n auto_https off\n}}\nhttp://:{TAILSCALE_PROXY_PORT} {{\n bind 127.0.0.1\n @probe path /_roost_proxy_{}\n respond @probe \"{}\" 200\n @private path /internal* /api/db-export*\n respond @private \"not found\" 404\n reverse_proxy 127.0.0.1:{} {{\n  header_up X-Forwarded-For unknown\n  header_up X-Forwarded-Proto https\n  header_up -Forwarded\n  header_up -X-Real-IP\n  header_up -X-Roost-On-Host\n  header_up -X-Roost-Remote-Addr\n }}\n}}\n",
        receipt.token, receipt.token, receipt.backend_port
    )
}

fn definition(
    controller: &TailscaleProxyController,
    receipt: &Receipt,
) -> Result<String, TailscaleError> {
    let config_path = controller.state.join("tailscale-proxy.caddyfile");
    let config = text_path(&config_path)?;
    match controller.platform {
        HostPlatform::Linux => Ok(format!(
            "[Unit]\nDescription=Roost Tailscale filtering proxy\n[Service]\nExecStart={} run --config {} --adapter caddyfile\nRestart=always\n[Install]\nWantedBy=default.target\n",
            quote_systemd(&receipt.caddy),
            quote_systemd(config)
        )),
        HostPlatform::MacOs => Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{LABEL}</string><key>ProgramArguments</key><array><string>{}</string><string>run</string><string>--config</string><string>{}</string><string>--adapter</string><string>caddyfile</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>\n",
            xml(&receipt.caddy),
            xml(config)
        )),
        HostPlatform::Windows => Err(TailscaleError::FilteringProxyRequired),
    }
}

fn file_matches(path: &Path, expected: &[u8], required: bool) -> Result<(), TailscaleError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => return Ok(()),
        Err(_) => return Err(TailscaleError::RecoveryRequired),
    };
    if !metadata.is_file() || metadata.len() > 16384 {
        return Err(TailscaleError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(TailscaleError::InsecureStateDirectory);
        }
    }
    if fs::read(path).map_err(|_| TailscaleError::ReceiptIo)? != expected {
        return Err(TailscaleError::Conflict);
    }
    Ok(())
}

pub(super) fn check_existing(
    controller: &TailscaleProxyController,
    receipt: &Receipt,
    required: bool,
) -> Result<(), TailscaleError> {
    file_matches(
        &controller.state.join("tailscale-proxy.caddyfile"),
        configuration(receipt).as_bytes(),
        required,
    )?;
    file_matches(
        &definition_path(controller)?,
        definition(controller, receipt)?.as_bytes(),
        required,
    )
}
pub(super) fn check_files(
    controller: &TailscaleProxyController,
    receipt: &Receipt,
) -> Result<(), TailscaleError> {
    check_existing(controller, receipt, true)
}
pub(super) fn any_files(controller: &TailscaleProxyController) -> Result<bool, TailscaleError> {
    for path in [
        controller.state.join("tailscale-proxy.caddyfile"),
        definition_path(controller)?,
    ] {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        }
    }
    Ok(false)
}

pub(super) fn create_private(path: &Path, bytes: &[u8]) -> Result<(), TailscaleError> {
    let parent = path.parent().ok_or(TailscaleError::ReceiptIo)?;
    let temporary = parent.join(format!(".roost-proxy-{}.pending", random_token()?));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| TailscaleError::ReceiptIo)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| TailscaleError::ReceiptIo)?;
    fs::hard_link(&temporary, path).map_err(|_| TailscaleError::ReceiptIo)?;
    fs::remove_file(&temporary).map_err(|_| TailscaleError::ReceiptIo)?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| TailscaleError::ReceiptIo)
}

#[cfg(unix)]
fn domain(controller: &TailscaleProxyController) -> Result<String, TailscaleError> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!(
        "gui/{}",
        fs::metadata(&controller.state)
            .map_err(|_| TailscaleError::ReceiptIo)?
            .uid()
    ))
}
#[cfg(not(unix))]
fn domain(_controller: &TailscaleProxyController) -> Result<String, TailscaleError> {
    Err(TailscaleError::FilteringProxyRequired)
}

pub(super) fn require_unoccupied(
    controller: &TailscaleProxyController,
) -> Result<(), TailscaleError> {
    match controller.platform {
        HostPlatform::Linux => {
            let load = command(
                "systemctl",
                &["--user", "show", "--property=LoadState", "--value", UNIT],
            )?;
            if load.trim() != "not-found" {
                return Err(TailscaleError::Conflict);
            }
        }
        HostPlatform::MacOs => {
            let name = format!("{}/{}", domain(controller)?, LABEL);
            if command("launchctl", &["print", &name]).is_ok() {
                return Err(TailscaleError::Conflict);
            }
        }
        HostPlatform::Windows => return Err(TailscaleError::FilteringProxyRequired),
    }
    if TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], TAILSCALE_PROXY_PORT)),
        Duration::from_millis(250),
    )
    .is_ok()
    {
        return Err(TailscaleError::Conflict);
    }
    Ok(())
}

fn manager_pid(controller: &TailscaleProxyController) -> Result<u32, TailscaleError> {
    let definition = definition_path(controller)?;
    let expected = text_path(&definition)?;
    match controller.platform {
        HostPlatform::Linux => {
            let path = command(
                "systemctl",
                &["--user", "show", "--property=FragmentPath", "--value", UNIT],
            )?;
            if path.trim() != expected {
                return Err(TailscaleError::Conflict);
            }
            let pid = command(
                "systemctl",
                &["--user", "show", "--property=MainPID", "--value", UNIT],
            )?;
            pid.trim()
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or(TailscaleError::CommandFailed)
        }
        HostPlatform::MacOs => {
            let name = format!("{}/{}", domain(controller)?, LABEL);
            let output = command("launchctl", &["print", &name])?;
            if !output.lines().any(|line| {
                line.trim()
                    .strip_prefix("path = ")
                    .is_some_and(|path| path == expected)
            }) {
                return Err(TailscaleError::Conflict);
            }
            output
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("pid = ")
                        .and_then(|pid| pid.parse::<u32>().ok())
                })
                .filter(|pid| *pid > 0)
                .ok_or(TailscaleError::CommandFailed)
        }
        HostPlatform::Windows => Err(TailscaleError::FilteringProxyRequired),
    }
}

pub(super) fn install(
    controller: &TailscaleProxyController,
    receipt: &Receipt,
) -> Result<(), TailscaleError> {
    check_existing(controller, receipt, false)?;
    let config_path = controller.state.join("tailscale-proxy.caddyfile");
    if !config_path.exists() {
        create_private(&config_path, configuration(receipt).as_bytes())?;
    }
    let path = definition_path(controller)?;
    let parent = path.parent().ok_or(TailscaleError::ReceiptIo)?;
    fs::create_dir_all(parent).map_err(|_| TailscaleError::ReceiptIo)?;
    if !path.exists() {
        create_private(&path, definition(controller, receipt)?.as_bytes())?;
    }
    check_files(controller, receipt)?;
    command(
        &receipt.caddy,
        &[
            "validate",
            "--config",
            text_path(&config_path)?,
            "--adapter",
            "caddyfile",
        ],
    )?;
    match controller.platform {
        HostPlatform::Linux => {
            command("systemctl", &["--user", "daemon-reload"])?;
            let loaded = command(
                "systemctl",
                &["--user", "show", "--property=FragmentPath", "--value", UNIT],
            )?;
            if loaded.trim() != text_path(&path)? {
                return Err(TailscaleError::Conflict);
            }
            command("systemctl", &["--user", "enable", "--now", UNIT])?;
        }
        HostPlatform::MacOs => {
            if manager_pid(controller).is_err() {
                command(
                    "launchctl",
                    &["bootstrap", &domain(controller)?, text_path(&path)?],
                )?;
            }
        }
        HostPlatform::Windows => return Err(TailscaleError::FilteringProxyRequired),
    }
    Ok(())
}

pub(super) fn verify_listener(
    controller: &TailscaleProxyController,
    receipt: &Receipt,
) -> Result<(), TailscaleError> {
    let pid = manager_pid(controller)?;
    let output = command("lsof", &["-nP", "-iTCP:4115", "-sTCP:LISTEN", "-t"])?;
    check_listener_pid(pid, &output)?;
    let response = request(&format!("/_roost_proxy_{}", receipt.token))?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or(TailscaleError::InvalidResponse)?;
    if !head.starts_with("HTTP/1.1 200 ") || body != receipt.token {
        return Err(TailscaleError::Conflict);
    }
    for path in [
        "/internal",
        "/internal/roost",
        "/api/db-export",
        "/api/db-export/roost",
    ] {
        if !request(path)?.starts_with("HTTP/1.1 404 ") {
            return Err(TailscaleError::Conflict);
        }
    }
    Ok(())
}
