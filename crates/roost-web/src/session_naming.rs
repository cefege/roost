//! What a terminal session is called and how old its activity reads: the
//! folder headline, the program subtitle, the tab/row title, and the compact
//! relative age. Ports `apps/web/src/lib/sessionTitle.ts` and
//! `apps/web/src/lib/relTime.ts`; the sidebar rows, the deck tabs and the close
//! labels read it. Paths go through `platform::worker_paths`.

use roost_client_core::Store;
use roost_protocol::wire::Session;
use unicode_segmentation::UnicodeSegmentation;

use crate::platform::worker_paths::{short_worker_path, worker_path_basename};

/// The longest title shown, in UTF-16 code units (v2's `MAX`).
pub const TITLE_MAX_UTF16: usize = 80;

/// Cut `text` to at most `max` UTF-16 code units at a grapheme boundary, so a
/// surrogate pair or a ZWJ cluster is dropped whole rather than split.
pub fn truncate_title(text: &str, max: usize) -> String {
    if text.encode_utf16().count() <= max {
        return text.to_owned();
    }
    let mut result = String::new();
    let mut used = 0;
    for cluster in text.graphemes(true) {
        let width = cluster.encode_utf16().count();
        if used + width > max {
            break;
        }
        result.push_str(cluster);
        used += width;
    }
    result
}

/// The user's rename, else the basename of the live cwd, else `Terminal`.
pub fn folder_headline(store: &Store, session: &Session) -> String {
    if let Some(custom) = custom_title(session) {
        return truncate_title(custom, TITLE_MAX_UTF16);
    }
    worker_path_basename(worker_os(store, session), &session.cwd)
        .filter(|basename| !basename.is_empty())
        .unwrap_or_else(|| "Terminal".to_owned())
}

/// The program's own OSC title, when it reported one.
pub fn program_subtitle(store: &Store, session: &Session) -> Option<String> {
    let title = store.terminal_titles.get(session.id.as_str())?.trim();
    (!title.is_empty()).then(|| truncate_title(title, TITLE_MAX_UTF16))
}

/// The rename, else the OSC title, else the short cwd, else `shell`: what the
/// sidebar row and the tab strip both read.
pub fn session_title(store: &Store, session: &Session) -> String {
    if let Some(custom) = custom_title(session) {
        return truncate_title(custom, TITLE_MAX_UTF16);
    }
    if let Some(subtitle) = program_subtitle(store, session) {
        return subtitle;
    }
    let short = short_worker_path(worker_os(store, session), &session.cwd);
    if short.is_empty() {
        "shell".to_owned()
    } else {
        short
    }
}

/// `12s`, `4m`, `3h`, `2d`: whole units since `then_ms`, never negative.
pub fn rel_time_since(now_ms: i64, then_ms: i64) -> String {
    let seconds = now_ms.saturating_sub(then_ms).max(0) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn custom_title(session: &Session) -> Option<&str> {
    session
        .custom_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
}

fn worker_os<'store>(store: &'store Store, session: &Session) -> Option<&'store str> {
    store
        .workers
        .get(session.worker_fp.as_str())
        .map(|worker| worker.os.as_str())
}
