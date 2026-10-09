//! The URL grammar: what path means what, and nothing that renders. Owned by
//! the SHELL slice (the one route authority) and depended on by the router, by
//! every component that links, and by the Playwright specs, which navigate by
//! these exact strings. Ports `apps/web/src/routes.ts`, including its concrete
//! href builders, the only sanctioned way to link to a parameterized route.
//!
//! The patterns are the route table the plan fixes — `/`, `/s/:sessionId`,
//! `/t/:workerFp/*folderPath`, the legacy `/w/:workspaceId` and
//! `/w/:workspaceId/t/:channelId`, `/a/:conversationId`, `/settings/:pane?`,
//! `/pair`, `/help`, `/design`, `/file/:workerFp/*path`, `/browse[/:workerFp]`,
//! `/search` — and
//! their rules are exercised through the public surface in
//! `crates/roost-web/tests/routes.rs`.

/// One route, and what the URL carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `/` — the session list and the workbench.
    Home,
    /// `/s/:sessionId` — one session's terminal.
    Session {
        /// The session to open.
        session_id: String,
    },
    /// `/a/:conversationId` — one built-in agent conversation's chat.
    Agent {
        /// The agent conversation to open.
        conversation_id: String,
    },
    /// `/t/:workerFp/*folderPath` — a local terminal in a folder on a machine.
    Terminal {
        /// The worker that owns the PTY.
        worker_fp: String,
        /// The folder's ROUTE splat, still route-encoded (no leading slash; a
        /// Windows root is tagged `~drive/`/`~unc/`). Decode it with
        /// `terminal_href::decode_folder_path`, which knows the worker's platform.
        folder_path: String,
    },
    /// `/w/:workspaceId` and `/w/:workspaceId/t/:channelId` — the legacy
    /// workspace form, kept so an old bookmark resolves instead of landing on
    /// the not-found page. It is a NAME for a session, not a second session
    /// route: `/w/...` is always resolved to the session it addresses before
    /// anything renders.
    Workspace {
        /// The legacy workspace id.
        workspace_id: String,
        /// The legacy channel id, when the URL named one.
        channel_id: Option<String>,
    },
    /// `/settings/:pane?` — the settings shell, optionally on one pane.
    Settings {
        /// The pane to open, when the URL named one.
        pane: Option<String>,
    },
    /// `/pair` — the pairing ceremony, and where a `#pair=` link lands.
    Pair,
    /// `/help`.
    Help,
    /// `/design` — the one visual reference: every token and every primitive.
    Design,
    /// `/file/:workerFp/*path` — a file in a worker's tree.
    File {
        /// The worker whose filesystem this is.
        worker_fp: String,
        /// The file's ROUTE splat, still route-encoded (no leading slash; a
        /// Windows root is tagged `~drive/`/`~unc/`). Decode it with
        /// `terminal_href::file_target`, which knows the worker's platform.
        path: String,
    },
    /// `/browse` or `/browse/:workerFp` — the machine and folder browser.
    Browse {
        /// The worker to browse, when the URL named one.
        worker_fp: Option<String>,
    },
    /// `/search` — global search.
    Search,
    /// A path that matches no route. Named rather than `None` so a caller has to
    /// decide what an unknown URL is instead of falling through to the home route
    /// and pretending the link worked.
    Unknown {
        /// The path, verbatim.
        path: String,
    },
}

impl Route {
    /// Every route this app answers on, in match order.
    ///
    /// Order matters and is why this is a list rather than a match on segment
    /// count: `/browse/:workerFp` and `/browse` differ only in a trailing
    /// segment, and `/settings/:pane?` differs only in an optional one.
    pub const ALL: &'static [Route] = &[
        Route::Home,
        Route::Session {
            session_id: String::new(),
        },
        Route::Agent {
            conversation_id: String::new(),
        },
        Route::Terminal {
            worker_fp: String::new(),
            folder_path: String::new(),
        },
        Route::Workspace {
            workspace_id: String::new(),
            channel_id: None,
        },
        Route::Settings { pane: None },
        Route::Pair,
        Route::Help,
        Route::Design,
        Route::File {
            worker_fp: String::new(),
            path: String::new(),
        },
        Route::Browse { worker_fp: None },
        Route::Search,
    ];

    /// Read a path, with or without its query and fragment.
    ///
    /// A trailing slash is not significant — `/help` and `/help/` are the same
    /// page — and neither is percent-encoding, because every captured segment
    /// is decoded before it is handed on. A route SPLAT is the exception: it is
    /// handed on as the route wrote it, because the route codec owns its
    /// encoding and un-encoding it here would decode it twice.
    pub fn parse(path: &str) -> Route {
        let path = path.split(['?', '#']).next().unwrap_or(path);
        let path = path.trim_end_matches('/');
        let path = if path.is_empty() { "/" } else { path };
        let segments: Vec<&str> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        let decoded: Vec<String> = segments.iter().map(|segment| decode(segment)).collect();

        match decoded.first().map(String::as_str) {
            None => Route::Home,
            Some("s") if decoded.len() == 2 => Route::Session {
                session_id: decoded[1].clone(),
            },
            Some("a") if decoded.len() == 2 => Route::Agent {
                conversation_id: decoded[1].clone(),
            },
            Some("t") => match (decoded.get(1), decoded.get(2)) {
                (Some(worker_fp), Some(_)) => Route::Terminal {
                    worker_fp: worker_fp.clone(),
                    folder_path: segments[2..].join("/"),
                },
                _ => Route::Unknown {
                    path: path.to_string(),
                },
            },
            Some("w") if decoded.len() == 2 => Route::Workspace {
                workspace_id: decoded[1].clone(),
                channel_id: None,
            },
            Some("w") if decoded.len() == 4 && decoded[2] == "t" => Route::Workspace {
                workspace_id: decoded[1].clone(),
                channel_id: Some(decoded[3].clone()),
            },
            Some("settings") if decoded.len() <= 2 => Route::Settings {
                pane: decoded.get(1).cloned(),
            },
            Some("pair") if decoded.len() == 1 => Route::Pair,
            Some("help") if decoded.len() == 1 => Route::Help,
            Some("design") if decoded.len() == 1 => Route::Design,
            Some("file") => match decoded.get(1) {
                Some(worker_fp) if decoded.len() > 2 => Route::File {
                    worker_fp: worker_fp.clone(),
                    path: segments[2..].join("/"),
                },
                _ => Route::Unknown {
                    path: path.to_string(),
                },
            },
            Some("browse") => match decoded.len() {
                1 => Route::Browse { worker_fp: None },
                2 => Route::Browse {
                    worker_fp: decoded.get(1).cloned(),
                },
                _ => Route::Unknown {
                    path: path.to_string(),
                },
            },
            Some("search") if decoded.len() == 1 => Route::Search,
            _ => Route::Unknown {
                path: path.to_string(),
            },
        }
    }

    /// The path this route is written as, with its captures filled in.
    ///
    /// The inverse of `parse` for every route that carries a capture, which is
    /// what makes a link built from a route and a link typed by a reader the same
    /// string.
    pub fn to_path(&self) -> String {
        match self {
            Self::Home => "/".to_string(),
            Self::Session { session_id } => format!("/s/{session_id}"),
            Self::Agent { conversation_id } => format!("/a/{conversation_id}"),
            Self::Terminal {
                worker_fp,
                folder_path,
            } => format!("/t/{worker_fp}/{folder_path}"),
            Self::Workspace {
                workspace_id,
                channel_id,
            } => match channel_id {
                Some(channel_id) => format!("/w/{workspace_id}/t/{channel_id}"),
                None => format!("/w/{workspace_id}"),
            },
            Self::Settings { pane } => match pane {
                Some(pane) => format!("/settings/{pane}"),
                None => "/settings".to_string(),
            },
            Self::Pair => "/pair".to_string(),
            Self::Help => "/help".to_string(),
            Self::Design => "/design".to_string(),
            Self::File { worker_fp, path } => format!("/file/{worker_fp}/{path}"),
            Self::Browse { worker_fp } => match worker_fp {
                Some(worker_fp) => format!("/browse/{worker_fp}"),
                None => "/browse".to_string(),
            },
            Self::Search => "/search".to_string(),
            Self::Unknown { path } => path.clone(),
        }
    }
}

/// `/s/:sessionId` for one session.
pub fn session_href(session_id: &str) -> String {
    Route::Session {
        session_id: session_id.to_owned(),
    }
    .to_path()
}

/// `/a/:conversationId` for one built-in agent conversation.
pub fn agent_href(conversation_id: &str) -> String {
    Route::Agent {
        conversation_id: conversation_id.to_owned(),
    }
    .to_path()
}

/// `/browse/:workerFp` for one machine.
pub fn browse_href(worker_fp: &str) -> String {
    Route::Browse {
        worker_fp: Some(worker_fp.to_owned()),
    }
    .to_path()
}

/// `/settings/:pane` for one settings pane.
pub fn settings_pane_href(pane: &str) -> String {
    Route::Settings {
        pane: Some(pane.to_owned()),
    }
    .to_path()
}

/// Percent-decode one path segment, leaving a malformed escape as it was.
///
/// A worker fingerprint and a session id are hex, so this normally has nothing to
/// do; a folder path with a literal `%` in it is the case that must not turn
/// into an empty segment.
fn decode(segment: &str) -> String {
    let Some(decoded) = percent_decode(segment) else {
        return segment.to_string();
    };
    decoded
}

/// Percent-decode one value, leaving a malformed escape as it was.
///
/// `None` for an escape that is not two hex digits, which is the same answer
/// [`decode`] gives a path segment: the raw text is more useful than a value
/// that silently lost characters.
pub fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            decoded.push(u8::from_str_radix(value.get(index + 1..index + 3)?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Percent-encode one query-string value, keeping it inside the grammar.
///
/// The inverse of [`percent_decode`], and the reason a query belongs to this
/// module: `/search?q=a%26b` has to read back as the text `a&b`, and a value
/// written raw would split into two parameters. Only the characters that would
/// change a parameter's meaning are escaped, so an ordinary title stays
/// legible in the address bar.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}
