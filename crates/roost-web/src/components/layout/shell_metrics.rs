//! Every decision the workbench chrome makes from a pathname and a viewport,
//! as pure functions. Ported from `apps/web/src/browser/windowSizeClass.ts`,
//! `apps/web/src/lib/workbenchTitle.ts` and the two projection memos in
//! `components/layout/WorkbenchStatusBar.tsx`.
//!
//! Nothing here reads a clock, a DOM node or the store. The components call
//! these with values they already hold, which is what lets a size-class
//! boundary, a title fallback and a coordinator staleness window each be a test
//! rather than a rendering accident. `AppShell` composes the results; the CSS in
//! `assets/styles/workbench-shell.css` is what draws them.

/// Material 3's window size class, at the boundaries the whole SPA uses.
///
/// Named for the layout decision, not the number: the desktop/compact split is
/// the one the shell acts on, and a caller that wanted the tablet class has to
/// say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    /// Short side under the compact boundary: the drawer replaces the rail.
    Compact,
    /// Wide enough for the persistent rail.
    Desktop,
}

/// Short side below which the window is compact.
///
/// Keyed on the SHORT side, not the width, so a phone held in landscape still
/// classifies compact and gets the full-screen sidebar. An iPad's short side is
/// above this, so it stays desktop.
pub const COMPACT_MAX_PX: u32 = 600;

/// Classify a viewport.
pub fn classify(width: u32, height: u32) -> SizeClass {
    if width.min(height) < COMPACT_MAX_PX {
        SizeClass::Compact
    } else {
        SizeClass::Desktop
    }
}

/// Which activity-rail destination a pathname selects.
///
/// A destination is a PREFIX match, exactly as v2's memos were: `/settings/machines`
/// and `/settings` are the same destination, and a path that matches none of them
/// highlights nothing rather than defaulting to the first item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Sessions,
    Search,
    Files,
    Settings,
    Help,
}

impl Destination {
    /// Every destination, in rail order, with the path it navigates to.
    ///
    /// The list is the rail's only source: a destination not in it cannot be
    /// drawn, so adding one is adding a row here and nowhere else.
    pub const ALL: [Destination; 5] = [
        Destination::Sessions,
        Destination::Search,
        Destination::Files,
        Destination::Settings,
        Destination::Help,
    ];

    /// The Material Symbols ligature the rail draws for this destination.
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Sessions => "terminal",
            Self::Search => "search",
            Self::Files => "folder_open",
            Self::Settings => "settings",
            Self::Help => "help",
        }
    }

    /// The word the rail's label and `aria-label` both read.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Search => "Search",
            Self::Files => "Files",
            Self::Settings => "Settings",
            Self::Help => "Help",
        }
    }

    /// Where this destination navigates to.
    ///
    /// Settings goes to the machines pane rather than the settings root, because
    /// the rail is a shortcut to the pane a reader wants and the settings root
    /// is an index they did not ask for.
    pub fn href(self) -> String {
        match self {
            Self::Sessions => "/".to_string(),
            Self::Search => "/search".to_string(),
            Self::Files => "/browse".to_string(),
            Self::Settings => "/settings/machines".to_string(),
            Self::Help => "/help".to_string(),
        }
    }

    /// The `data-testid` the Playwright specs address this destination by.
    pub const fn test_id(self) -> &'static str {
        match self {
            Self::Sessions => "workbench-activity-sessions",
            Self::Search => "workbench-activity-search",
            Self::Files => "workbench-activity-files",
            Self::Settings => "workbench-activity-settings",
            Self::Help => "workbench-activity-help",
        }
    }

    /// Whether a pathname selects this destination.
    ///
    /// Sessions is the odd one: it owns the three terminal route prefixes as
    /// well as the root, because every terminal route is a session and a rail
    /// that greyed out Sessions while a terminal was open would be lying.
    pub fn is_active(self, pathname: &str) -> bool {
        match self {
            Self::Sessions => {
                pathname == "/"
                    || pathname.starts_with("/s/")
                    || pathname.starts_with("/t/")
                    || pathname.starts_with("/w/")
            }
            Self::Search => pathname.starts_with("/search"),
            Self::Files => pathname.starts_with("/file/") || pathname.starts_with("/browse"),
            Self::Settings => pathname.starts_with("/settings"),
            Self::Help => pathname.starts_with("/help"),
        }
    }
}

/// The one destination a pathname selects, when exactly one does.
pub fn active_destination(pathname: &str) -> Option<Destination> {
    Destination::ALL
        .into_iter()
        .find(|destination| destination.is_active(pathname))
}

/// The context text the title region and the compact bar both read.
///
/// A session's own title wins, then the basename of the folder it runs in, then
/// `Terminal` for a terminal route that resolved to no session, then `Roost`.
/// The order is the whole rule: a route that names a live session must not read
/// as a generic surface, and a route that names none must not read as a session.
pub fn workbench_title(
    pathname: &str,
    session_title: Option<&str>,
    session_folder: Option<&str>,
) -> String {
    if pathname.starts_with("/search") {
        return "Search".to_string();
    }
    if pathname.starts_with("/file/") {
        return "Files".to_string();
    }
    if pathname.starts_with("/settings") {
        return "Settings".to_string();
    }
    if pathname.starts_with("/help") {
        return "Help".to_string();
    }
    // ONLY A TERMINAL ROUTE HAS A SESSION TO NAME. The root is the workbench, and
    // `/settings` is the settings shell: neither resolves to a session, so a
    // store that still holds one from a previous route must not put that
    // session's name in the title bar. v2 gets this by construction —
    // `activeSessionForPath` returns null for both — and the port has to make the
    // same exclusion explicit, because here the session arrives as an argument
    // rather than as something the path looked up.
    let is_terminal =
        pathname.starts_with("/s/") || pathname.starts_with("/t/") || pathname.starts_with("/w/");
    if is_terminal && (session_title.is_some() || session_folder.is_some()) {
        let title = session_title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(|title| title.chars().take(TITLE_MAX_CHARS).collect::<String>());
        if let Some(title) = title {
            return title;
        }
        return folder_basename(session_folder.unwrap_or_default())
            .filter(|basename| !basename.is_empty())
            .unwrap_or_else(|| "~".to_string());
    }
    if pathname.starts_with("/s/") || pathname.starts_with("/t/") || pathname.starts_with("/w/") {
        return "Terminal".to_string();
    }
    "Roost".to_string()
}

/// How much of a session title the title region shows.
///
/// A long agent-set title would push the brand out of the title bar, and the
/// brand is the one control that always means "go home".
pub const TITLE_MAX_CHARS: usize = 60;

/// The last path segment of a folder, with a trailing separator already dropped.
///
/// `/home/ada/src/` is `src`, and `/home/ada` is `ada`. An empty folder has no
/// basename, which the caller reads as "fall through" rather than as a title.
pub fn folder_basename(folder: &str) -> Option<String> {
    let trimmed = folder.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(str::to_string)
}

/// How long without a successful coordinator round trip before the status bar
/// calls the coordinator unreachable, while this tab is visible.
///
/// v2's window. A shorter one turns a busy coordinator red; a longer one leaves
/// a dead one green.
pub const COORD_STALE_MS: i64 = 10_000;

/// What the status bar says about the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorState {
    /// This browser has no route to the network.
    Offline,
    /// The last attempt failed, or the last success is older than the window.
    Unreachable,
    /// Reachable, but no round trip has completed yet.
    Syncing,
    /// Reachable, and a round trip completed recently enough.
    Synced,
}

impl CoordinatorState {
    /// The dot this state shows. The vocabulary is the design system's, so the
    /// status bar and the agent chips cannot invent a second set of colours.
    pub const fn status(self) -> &'static str {
        match self {
            Self::Offline | Self::Unreachable => "offline",
            Self::Syncing => "idle",
            Self::Synced => "ok",
        }
    }

    /// The word beside the dot.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Unreachable => "Coordinator unreachable",
            Self::Syncing => "Syncing",
            Self::Synced => "Synced",
        }
    }
}

/// The inputs the coordinator state is decided from.
///
/// The clock and the page-visibility flag are parameters rather than reads so
/// that "the coordinator went stale" is a test with a fixed `now_ms`, and so a
/// backgrounded tab does not paint itself red for a window it was never
/// watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatorHealth {
    /// Whether the browser itself has a route to the network. A browser that
    /// says it is offline is not waiting on a coordinator, whatever the link
    /// last said.
    pub offline: bool,
    /// Whether the last attempt failed outright.
    pub last_attempt_failed: bool,
    /// When the last attempt succeeded, or `None` when none has.
    pub last_success_ms: Option<i64>,
    /// Whether this tab is in the foreground. A hidden tab is not judged stale.
    pub page_visible: bool,
    /// The clock, in the same epoch as `last_success_ms`.
    pub now_ms: i64,
}

/// The status bar's health, read off the live Sync link.
///
/// v2 polls `misc.health` every five seconds and publishes the answer on
/// `window`; this build runs no poller and needs none, because the link carries
/// the same evidence and carries it continuously. A link that is open says the
/// coordinator is answering, and how long ago it last said so is exactly the
/// staleness the window is about. A second timer re-asking a socket the store
/// already watches would be a second source of truth about one link.
///
/// `link_idle_ms` is the Sync link's own answer — `Some` exactly while a socket
/// is open — so there is no second notion of "is the coordinator up" to keep in
/// step with it.
#[must_use]
pub fn coordinator_health_from_link(
    link_idle_ms: Option<u64>,
    now_ms: u64,
    offline: bool,
    page_visible: bool,
) -> CoordinatorHealth {
    CoordinatorHealth {
        offline,
        last_attempt_failed: link_idle_ms.is_none(),
        last_success_ms: link_idle_ms.map(|idle| now_ms.saturating_sub(idle) as i64),
        page_visible,
        now_ms: now_ms as i64,
    }
}

/// Decide what the status bar says about the coordinator.
pub fn coordinator_state(identity_known: bool, health: CoordinatorHealth) -> CoordinatorState {
    // The browser's own verdict comes first: a machine with no route to the
    // network is not waiting on a coordinator, and naming the coordinator
    // would send an operator to restart the one thing that is working.
    if health.offline {
        return CoordinatorState::Offline;
    }
    if !health.page_visible {
        // A hidden tab is neither proven nor disproven. Reporting `Synced` from
        // the last known answer is the honest reading; reporting `Unreachable`
        // would alarm a reader about a tab they cannot see failing.
        return match (health.last_attempt_failed, health.last_success_ms) {
            (false, Some(_)) => CoordinatorState::Synced,
            _ => CoordinatorState::Syncing,
        };
    }
    let stale = health
        .last_success_ms
        .is_some_and(|last| health.now_ms.saturating_sub(last) > COORD_STALE_MS);
    if stale || health.last_attempt_failed {
        return CoordinatorState::Unreachable;
    }
    if !identity_known || health.last_success_ms.is_none() {
        return CoordinatorState::Syncing;
    }
    CoordinatorState::Synced
}

/// The session context the status bar shows: the title, and the folder when the
/// folder is not already the title.
///
/// One string rather than two items, because the pair is read as a unit and two
/// items would let the folder appear alone, which reads as a path with no
/// session behind it.
pub fn session_context(title: &str, folder: Option<&str>) -> String {
    match folder {
        Some(folder) if !folder.is_empty() && folder != title => format!("{title} · {folder}"),
        _ => title.to_string(),
    }
}
