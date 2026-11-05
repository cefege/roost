//! One safe link for OMP's semantic surfaces, and the resolver host services
//! share with it.
//!
//! External `http`, `https` and `mailto` targets become browser anchors. A
//! worker path or `file:` URI goes through the canonical terminal parser and
//! `terminal_file_link::resolve_terminal_file`, so the reader lands on the
//! authenticated `/file/…` viewer instead of a raw `file://` URL that opens
//! their own disk. Anything else stays inert text.

use dioxus::prelude::*;
use roost_protocol::cell::link_uri_within_cap;
use roost_protocol::native_dashboard::{capability_path, parse_capability_path};
use roost_web_terminal::link_target::{
    TerminalLinkTarget, classify_terminal_link_target, is_worker_file_href,
};

use crate::components::agent_chat::semantic_ui::media::MediaContext;
use crate::pump::use_store;
use crate::router_state::use_navigate;
use crate::terminal_file_link::resolve_terminal_file;
use crate::terminal_href::worker_os;

/// Where one semantic link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeTarget {
    /// A browser-owned URL. The scheme is the parsed one, never assumed.
    External {
        /// The exact authored URL.
        href: String,
        /// `http`, `https` or `mailto`.
        scheme: &'static str,
    },
    /// An authenticated worker-file route.
    WorkerFile {
        /// `/file/<workerFp>/<path>[#L<line>]`.
        href: String,
    },
    /// A target no safe handler accepts; render its text.
    Inert,
}

/// Classify an OMP-authored link against the conversation's worker and cwd.
///
/// The file path rules are `roost_web_terminal::link_target`'s, so percent
/// decoding, `file://host` authority handling, `:line` suffixes and the
/// `/file/…` route check are shared with every terminal link.
pub(crate) fn resolve_native_target(
    raw: &str,
    worker_fp: &str,
    cwd: &str,
    worker_os: Option<&str>,
) -> NativeTarget {
    if safe_mailto(raw) {
        return NativeTarget::External {
            href: raw.to_owned(),
            scheme: "mailto",
        };
    }
    if is_worker_file_href(raw) {
        return NativeTarget::WorkerFile {
            href: raw.to_owned(),
        };
    }
    let resolve = |raw_path: &str, line: Option<u64>, authority: Option<&str>| {
        resolve_terminal_file(worker_os, worker_fp, cwd, raw_path, line, authority)
    };
    match classify_terminal_link_target(raw, Some(&resolve)) {
        Some(TerminalLinkTarget::External { href, .. }) => {
            let scheme = if href
                .get(..8)
                .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
            {
                "https"
            } else {
                "http"
            };
            NativeTarget::External { href, scheme }
        }
        Some(TerminalLinkTarget::WorkerFile {
            href: Some(href), ..
        }) => NativeTarget::WorkerFile { href },
        Some(TerminalLinkTarget::WorkerFile { href: None, .. }) | None => NativeTarget::Inert,
    }
}

/// `mailto:` has no host for the shared parser to check, so its address is
/// validated here: non-empty mailbox, no whitespace or control characters, and
/// inside the URI cap every terminal link uses.
fn safe_mailto(raw: &str) -> bool {
    if !link_uri_within_cap(raw)
        || raw.is_empty()
        || raw
            .chars()
            .any(|character| character <= ' ' || character == '\u{7f}')
    {
        return false;
    }
    let Some(head) = raw.get(..7) else {
        return false;
    };
    if !head.eq_ignore_ascii_case("mailto:") {
        return false;
    }
    let address = &raw[7..];
    let (mailbox, _) = address.split_once('?').unwrap_or((address, ""));
    !mailbox.is_empty()
}

/// What a host `open_url` request may actually open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostOpenTarget {
    /// A real http(s) URL for the browser.
    Browser(String),
    /// An authenticated worker file route for this document.
    WorkerFile(String),
}

/// Resolve a native `open_url` value.
///
/// The native adapter sends http(s) URLs and worker-local paths or `file:`
/// URLs. A worker's own localhost callback is not this browser's localhost, so
/// only a validated route or an absolute http(s) URL is accepted; anything
/// else is a concrete refusal the caller shows to the operator.
pub(crate) fn resolve_host_open(
    value: &str,
    worker_fp: &str,
    cwd: &str,
    worker_os: Option<&str>,
) -> Result<HostOpenTarget, String> {
    match resolve_native_target(value, worker_fp, cwd, worker_os) {
        NativeTarget::External {
            href,
            scheme: "http" | "https",
        } => {
            if let Some(host) = loopback_host(&href) {
                return Err(format!(
                    "OMP asked to open a callback on the worker's own {host}. This browser                      cannot reach the worker's localhost; use the provider's manual-code sign-in                      instead."
                ));
            }
            Ok(HostOpenTarget::Browser(href))
        }
        NativeTarget::External { scheme, .. } => Err(format!(
            "OMP asked to open a {scheme} URL, which this browser cannot open safely."
        )),
        NativeTarget::WorkerFile { href } => Ok(HostOpenTarget::WorkerFile(href)),
        NativeTarget::Inert => Err(
            "OMP asked to open a target this browser cannot reach. Only http(s) links and \
             worker files are supported."
                .to_owned(),
        ),
    }
}

/// Validate the one URL a native stats prompt may open.
///
/// The coordinator already replaced the worker-local dashboard URL with an
/// opaque capability path. This accepts exactly that path and nothing that
/// could reach another origin, carry a query the bridge did not mint, or point
/// at a cohort of resources other than the dashboard itself.
pub(crate) fn resolve_stats_capability(value: &str) -> Result<String, String> {
    let Some((token, upstream)) = parse_capability_path(value) else {
        return Err("The stats dashboard capability is malformed.".to_owned());
    };
    if upstream != "/" || value != capability_path(token) {
        return Err("The stats dashboard capability is not the minted address.".to_owned());
    }
    Ok(value.to_owned())
}

/// The loopback authority of an absolute http(s) URL, when it names one.
///
/// A native OAuth callback is handed back as `http://127.0.0.1:<port>/…` on the
/// WORKER. Opening that in the browser reaches the reader's own machine, not the
/// worker, so it is refused with the concrete prerequisite instead.
fn loopback_host(href: &str) -> Option<&str> {
    let rest = href
        .strip_prefix("http://")
        .or_else(|| href.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, tail)| tail);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split_once(']')?.0
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let lower = host.to_ascii_lowercase();
    let is_loopback = lower == "localhost"
        || lower == "::1"
        || lower == "0.0.0.0"
        || lower.starts_with("127.")
        || lower.ends_with(".localhost");
    is_loopback.then_some(host)
}

/// A safe semantic link.
///
/// Reads the surface's `MediaContext` for the owning worker and cwd, exactly as
/// media previews do, so a link resolves against the same machine. A worker
/// file navigates in-app; an external URL opens a new tab with no opener.
#[component]
pub(crate) fn NativeLink(
    href: String,
    #[props(default)] class: Option<String>,
    #[props(default)] title: Option<String>,
    children: Element,
) -> Element {
    let media_context = use_context::<Signal<MediaContext>>();
    let context = (*media_context.read()).clone();
    let pump = use_store();
    let navigate = use_navigate();
    let worker_os = {
        let core = pump.core();
        let core = core.borrow();
        worker_os(core.store(), &context.worker_fp).map(str::to_owned)
    };
    let target = resolve_native_target(
        &href,
        &context.worker_fp,
        &context.cwd,
        worker_os.as_deref(),
    );
    let class_attr = class.unwrap_or_default();
    let title_attr = title.unwrap_or_default();
    match target {
        NativeTarget::External {
            href,
            scheme: "mailto",
        } => rsx! {
            a {
                href: "{href}",
                class: "{class_attr}",
                title: "{title_attr}",
                {children}
            }
        },
        NativeTarget::External { href, .. } => rsx! {
            a {
                href: "{href}",
                target: "_blank",
                rel: "noopener noreferrer",
                class: "{class_attr}",
                title: "{title_attr}",
                {children}
            }
        },
        NativeTarget::WorkerFile { href } => {
            let navigate_to = href.clone();
            rsx! {
                a {
                    href: "{href}",
                    class: "{class_attr}",
                    title: "{title_attr}",
                    onclick: move |event: MouseEvent| {
                        event.prevent_default();
                        navigate.call(navigate_to.clone());
                    },
                    {children}
                }
            }
        }
        NativeTarget::Inert => rsx! {
            span { class: "{class_attr}", title: "{title_attr}", {children} }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn http_and_https_links_stay_browser_owned() {
        assert_eq!(
            resolve_native_target("https://example.com/a?b=1", WORKER, "/work", Some("linux")),
            NativeTarget::External {
                href: "https://example.com/a?b=1".into(),
                scheme: "https"
            }
        );
        assert_eq!(
            resolve_native_target("mailto:ops@example.com", WORKER, "/work", Some("linux")),
            NativeTarget::External {
                href: "mailto:ops@example.com".into(),
                scheme: "mailto"
            }
        );
    }

    #[test]
    fn unsafe_schemes_and_relative_text_stay_inert() {
        for raw in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "https://",
            "mailto:",
            "file:relative",
            "just some text",
        ] {
            assert_eq!(
                resolve_native_target(raw, WORKER, "/work", Some("linux")),
                NativeTarget::Inert,
                "{raw}"
            );
        }
    }

    #[test]
    fn worker_paths_use_the_authenticated_file_route_and_keep_the_line() {
        let NativeTarget::WorkerFile { href } = resolve_native_target(
            "/srv/roost/README.md:12",
            WORKER,
            "/srv/roost",
            Some("linux"),
        ) else {
            panic!("a worker path resolves");
        };
        assert!(href.starts_with("/file/"));
        assert!(href.ends_with("#L12"), "{href}");

        let NativeTarget::WorkerFile { href } = resolve_native_target(
            "file:///srv/roost/a%20b.txt",
            WORKER,
            "/srv/roost",
            Some("linux"),
        ) else {
            panic!("a file URI resolves");
        };
        assert!(
            href.contains("a%20b.txt") || href.contains("a b.txt"),
            "{href}"
        );
    }

    #[test]
    fn an_already_minted_file_route_is_not_resolved_again_against_the_cwd() {
        let route = format!("/file/{WORKER}/srv%2Froost%2FREADME.md#L4");
        assert_eq!(
            resolve_native_target(&route, WORKER, "/srv/roost", Some("linux")),
            NativeTarget::WorkerFile { href: route }
        );
    }

    #[test]
    fn host_open_refuses_non_http_external_schemes() {
        let refusal = resolve_host_open("mailto:ops@example.com", WORKER, "/work", Some("linux"))
            .expect_err("mailto is not a host service target");
        assert!(refusal.contains("mailto"));
        assert_eq!(
            resolve_host_open("https://example.com/cb", WORKER, "/work", Some("linux")),
            Ok(HostOpenTarget::Browser("https://example.com/cb".into()))
        );
        assert!(matches!(
            resolve_host_open("/work/file.txt", WORKER, "/work", Some("linux")),
            Ok(HostOpenTarget::WorkerFile(_))
        ));
        for callback in [
            "http://127.0.0.1:8976/callback",
            "http://localhost:8976/callback",
            "https://[::1]:8976/callback",
        ] {
            let refusal = resolve_host_open(callback, WORKER, "/work", Some("linux"))
                .expect_err("a worker-local callback is not this browser's localhost");
            assert!(
                refusal.contains("localhost") || refusal.contains("127.0.0.1"),
                "{refusal}"
            );
        }
    }

    #[test]
    fn only_a_minted_dashboard_capability_path_opens() {
        let token = "0123456789abcdef".repeat(4);
        let path = capability_path(&token);
        assert_eq!(resolve_stats_capability(&path), Ok(path.clone()));
        for refused in [
            "/omp-stats/".to_owned() + &token,
            format!("{path}api/stats"),
            format!("/omp-stats/{token}?x=1"),
            "http://127.0.0.1:3847/".to_owned(),
            format!("/omp-stats/{token}/../"),
        ] {
            assert!(resolve_stats_capability(&refused).is_err(), "{refused}");
        }
    }
}
