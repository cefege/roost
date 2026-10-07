//! Handing a minted browser grant to a person: open it in this machine's
//! browser, or print its link with a QR code a phone can scan.
//!
//! Called by `roost quickstart` and `roost add-browser`. Depends on
//! `roost_platform::browser_pairing_link` for the link shape the web client
//! captures, and on `pairing_qr` for the drawing.

use std::io::IsTerminal as _;

use roost_host::HostPlatform;

use crate::quickstart::add_machine::is_loopback_host;
use crate::quickstart::pairing_qr::render_terminal_qr;

/// Whether the QR is drawn beside a printed link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingQr {
    /// Draw it, when stderr is a terminal and a phone could reach the origin.
    Shown,
    /// `--no-qr`: the link alone.
    Suppressed,
}

impl PairingQr {
    /// The choice a `--no-qr` flag makes.
    pub fn from_no_qr_flag(no_qr: bool) -> Self {
        if no_qr { Self::Suppressed } else { Self::Shown }
    }
}

/// Print `link` on stdout, with its QR on stderr above it.
///
/// stdout carries the link and nothing else, so `$(roost add-browser)` still
/// captures exactly one URL. The QR is for a person, so it goes to stderr and
/// only when stderr is a terminal: a log file gets no block of escape codes. A
/// loopback origin gets a note instead of a code, because a phone that scans it
/// opens its own loopback and pairs nothing.
pub fn print_pairing_link(origin: &str, link: &str, qr: PairingQr) {
    if qr == PairingQr::Shown && std::io::stderr().is_terminal() {
        if is_loopback_host(origin) {
            eprintln!(
                "No QR: {origin} is this machine's loopback, which a phone cannot open. Declare \
                 ROOST_WEB_PUBLIC_URL to pair a phone by scanning."
            );
        } else {
            match render_terminal_qr(link) {
                Ok(drawn) => {
                    eprintln!("Scan with a phone camera to pair it:");
                    eprint!("{drawn}");
                }
                Err(error) => eprintln!("No QR: the link cannot be encoded ({error})."),
            }
        }
    }
    println!("{link}");
}

/// Open the pairing link in this machine's default browser.
///
/// The link is the declared front door when one was given, and the grant rides
/// in the fragment, so it stays in the page rather than in a server log. On
/// refusal the caller prints the link instead: an `Err` carries the sentence
/// that explains why.
pub fn open_paired_browser(platform: HostPlatform, link: &str) -> Result<(), String> {
    let opener = match platform {
        HostPlatform::MacOs => "open",
        HostPlatform::Linux => "xdg-open",
        HostPlatform::Windows => {
            return Err("This platform has no browser opener in Roost v3.".to_string());
        }
    };
    std::process::Command::new(opener)
        .arg(link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| format!("A browser could not be opened with {opener}."))
}

#[cfg(test)]
mod tests {
    use crate::quickstart::add_machine::is_loopback_host;

    #[test]
    fn a_loopback_origin_is_recognised_with_or_without_a_port() {
        for loopback in [
            "http://127.0.0.1:4113",
            "http://localhost:4113",
            "http://localhost",
            "https://roost.localhost:8443",
            "http://[::1]:4113",
        ] {
            assert!(is_loopback_host(loopback), "{loopback}");
        }
        for reachable in [
            "https://roost.example.com",
            "https://roost.example.com:8443",
            "http://100.64.0.7:4113",
            "https://localhost.example.com",
        ] {
            assert!(!is_loopback_host(reachable), "{reachable}");
        }
    }
}
