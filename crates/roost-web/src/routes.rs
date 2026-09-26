//! The URL grammar: what path means what, and nothing that renders.
//!
//! Owned by the track lead and depended on by the router, by every component
//! that links, and by the Playwright specs, which navigate by these exact
//! strings. The grammar is decided here so that a component never re-derives "is
//! this a terminal link or a file link" from the shape of a path.
//!
//! Patterns come from the route table the plan fixes: `/`, `/s/:sessionId`,
//! `/t/:workerFp/*folderPath`, `/settings/:pane?`, `/pair`, `/help`, `/design`,
//! `/file/:workerFp/*path`, `/browse[/:workerFp]`, `/search`.

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
    /// `/t/:workerFp/*folderPath` — a local terminal in a folder on a machine.
    Terminal {
        /// The worker that owns the PTY.
        worker_fp: String,
        /// The folder, as `/`-separated segments with no leading slash.
        folder_path: String,
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
        /// The path inside that filesystem.
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
        Route::Terminal {
            worker_fp: String::new(),
            folder_path: String::new(),
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
    /// page — and neither is percent-encoding, because every captured segment is
    /// decoded before it is handed on.
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
            Some("t") => match (decoded.get(1), decoded.get(2)) {
                (Some(worker_fp), Some(_)) => Route::Terminal {
                    worker_fp: worker_fp.clone(),
                    folder_path: decoded[2..].join("/"),
                },
                _ => Route::Unknown {
                    path: path.to_string(),
                },
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
                    path: decoded[2..].join("/"),
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
            Self::Terminal {
                worker_fp,
                folder_path,
            } => format!("/t/{worker_fp}/{folder_path}"),
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

fn percent_decode(value: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::Route;

    #[test]
    fn the_root_is_the_workbench_and_a_trailing_slash_is_the_same_page() {
        assert_eq!(Route::parse("/"), Route::Home);
        assert_eq!(Route::parse(""), Route::Home);
        assert_eq!(Route::parse("/?tab=1"), Route::Home);
    }

    #[test]
    fn a_session_path_carries_the_session_and_nothing_else() {
        assert_eq!(
            Route::parse("/s/abc123"),
            Route::Session {
                session_id: "abc123".to_string()
            }
        );
        // A second segment is not a session id; it is a different URL, and
        // answering it with the first session's page is how a mistyped link
        // opens the wrong terminal.
        assert!(matches!(
            Route::parse("/s/abc123/extra"),
            Route::Unknown { .. }
        ));
    }

    #[test]
    fn a_terminal_path_keeps_the_whole_folder_below_the_worker() {
        // The folder is a catch-all: a path segment is legal in a folder name and
        // truncating at the first one sends the terminal somewhere else.
        assert_eq!(
            Route::parse("/t/aa11/src/deep/nested/folder"),
            Route::Terminal {
                worker_fp: "aa11".to_string(),
                folder_path: "src/deep/nested/folder".to_string()
            }
        );
    }

    #[test]
    fn a_settings_pane_is_optional_and_nothing_more() {
        assert_eq!(Route::parse("/settings"), Route::Settings { pane: None });
        assert_eq!(
            Route::parse("/settings/machines"),
            Route::Settings {
                pane: Some("machines".to_string())
            }
        );
        assert!(matches!(
            Route::parse("/settings/a/b"),
            Route::Unknown { .. }
        ));
    }

    #[test]
    fn browse_is_optional_too_and_never_guesses_a_worker() {
        assert_eq!(Route::parse("/browse"), Route::Browse { worker_fp: None });
        assert_eq!(
            Route::parse("/browse/aa11"),
            Route::Browse {
                worker_fp: Some("aa11".to_string())
            }
        );
    }

    #[test]
    fn an_unknown_path_is_named_rather_than_falling_through_to_home() {
        let parsed = Route::parse("/nope");
        assert_eq!(
            parsed,
            Route::Unknown {
                path: "/nope".to_string()
            }
        );
        // The round trip is what makes a not-found page able to show the URL it
        // could not answer.
        assert_eq!(parsed.to_path(), "/nope");
    }

    #[test]
    fn a_file_path_needs_both_a_worker_and_a_path() {
        assert_eq!(
            Route::parse("/file/aa11/etc/hosts"),
            Route::File {
                worker_fp: "aa11".to_string(),
                path: "etc/hosts".to_string()
            }
        );
        assert!(matches!(Route::parse("/file/aa11"), Route::Unknown { .. }));
    }

    #[test]
    fn every_route_with_captures_round_trips_through_its_own_path() {
        // A link built from a route and a link typed by a reader have to be the
        // same string, or a copied URL stops working.
        let routes = [
            Route::Home,
            Route::Session {
                session_id: "abc".to_string(),
            },
            Route::Terminal {
                worker_fp: "aa11".to_string(),
                folder_path: "src/deep".to_string(),
            },
            Route::Settings { pane: None },
            Route::Settings {
                pane: Some("machines".to_string()),
            },
            Route::Pair,
            Route::Help,
            Route::Design,
            Route::File {
                worker_fp: "aa11".to_string(),
                path: "etc/hosts".to_string(),
            },
            Route::Browse { worker_fp: None },
            Route::Browse {
                worker_fp: Some("aa11".to_string()),
            },
            Route::Search,
        ];
        for route in routes {
            assert_eq!(Route::parse(&route.to_path()), route, "{route:?}");
        }
    }
}
