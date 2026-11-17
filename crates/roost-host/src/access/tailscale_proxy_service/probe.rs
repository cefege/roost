//! Command execution and loopback probes for the Tailscale proxy.
//! Listener checks are tied to the service manager's process identity.
//! Used by the service owner to verify the fixed local listener.

use super::*;
use std::{
    io::Read,
    process::{Command, Stdio},
    time::Instant,
};

pub(super) fn command(program: &str, arguments: &[&str]) -> Result<String, TailscaleError> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| TailscaleError::CommandFailed)?;
    let mut stdout = child.stdout.take().ok_or(TailscaleError::CommandFailed)?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = (&mut stdout).take(16385).read_to_end(&mut data);
        let _ = sender.send(result.map(|_| data));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TailscaleError::CommandFailed);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TailscaleError::CommandTimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let bytes = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| TailscaleError::CommandTimedOut)?
        .map_err(|_| TailscaleError::CommandFailed)?;
    if !status.success() || bytes.len() > 16384 {
        return Err(TailscaleError::CommandFailed);
    }
    String::from_utf8(bytes).map_err(|_| TailscaleError::InvalidResponse)
}

pub(super) fn request(path: &str) -> Result<String, TailscaleError> {
    let address = SocketAddr::from(([127, 0, 0, 1], TAILSCALE_PROXY_PORT));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|_| TailscaleError::CommandFailed)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| TailscaleError::CommandFailed)?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| TailscaleError::CommandFailed)?;
    let mut response = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TailscaleError::CommandTimedOut);
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| TailscaleError::CommandFailed)?;
        let mut buffer = [0u8; 1024];
        let count = stream
            .read(&mut buffer)
            .map_err(|_| TailscaleError::CommandFailed)?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..count]);
        if response.len() > 8192 {
            return Err(TailscaleError::InvalidResponse);
        }
    }
    String::from_utf8(response).map_err(|_| TailscaleError::InvalidResponse)
}

pub(super) fn check_listener_pid(pid: u32, output: &str) -> Result<(), TailscaleError> {
    let pids: Vec<u32> = output
        .lines()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| TailscaleError::InvalidResponse)?;
    if pid == 0 || pids.as_slice() != [pid] {
        return Err(TailscaleError::Conflict);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_proxy_filters_private_paths_and_replaces_forwarding_claims() {
        let receipt = Receipt {
            backend_port: 4113,
            token: "0123456789abcdef0123456789abcdef".into(),
            caddy: "/usr/bin/caddy".into(),
        };
        let config = configuration(&receipt);
        assert!(config.contains("bind 127.0.0.1"));
        assert!(config.contains("@private path /internal* /api/db-export*"));
        assert!(config.contains("header_up X-Forwarded-For unknown"));
        assert!(config.contains("header_up -Forwarded"));
        assert!(config.contains("reverse_proxy 127.0.0.1:4113"));
        assert!(!config.contains("tls"));
    }

    #[test]
    fn service_paths_are_escaped_for_their_manager() {
        assert_eq!(quote_systemd("/a%\"b\\c"), "\"/a%%\\\"b\\\\c\"");
        assert_eq!(xml("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
    }

    #[test]
    fn listener_must_have_exactly_the_service_manager_pid() {
        assert!(check_listener_pid(42, "42\n").is_ok());
        assert!(check_listener_pid(42, "").is_err());
        assert!(check_listener_pid(42, "41\n").is_err());
        assert!(check_listener_pid(42, "42\n43\n").is_err());
        assert!(check_listener_pid(42, "not-a-pid\n").is_err());
    }

    #[test]
    fn interrupted_proxy_install_accepts_missing_but_not_changed_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let controller =
            TailscaleProxyController::new(directory.path(), directory.path(), HostPlatform::Linux);
        fs::create_dir(&controller.state).expect("state directory");
        let receipt = Receipt {
            backend_port: 4113,
            token: "0123456789abcdef0123456789abcdef".into(),
            caddy: "/usr/bin/caddy".into(),
        };
        create_private(
            &controller.state.join("tailscale-proxy.caddyfile"),
            configuration(&receipt).as_bytes(),
        )
        .expect("config");
        assert!(check_existing(&controller, &receipt, false).is_ok());
        assert!(check_files(&controller, &receipt).is_err());
        fs::write(
            controller.state.join("tailscale-proxy.caddyfile"),
            "changed",
        )
        .expect("changed config");
        assert!(check_existing(&controller, &receipt, false).is_err());
    }

    #[test]
    fn caddy_executable_is_strictly_allowlisted() {
        assert!(matches!(
            check_prerequisites("/tmp/caddy"),
            Err(TailscaleError::CaddyUnavailable)
        ));
    }
}
