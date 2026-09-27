//! Which listed names a machine shows, in which order, and how a modification
//! time reads as relative text.
//!
//! Entries arrive keyed by MACHINE and this file never sees the machine: it
//! answers "given these entries, what does the list show", and the machine is
//! the caller's business because the caller is the one holding the machine's
//! listing. A file name is not an identity, and neither is a path — two
//! machines can both have `~/src`, and this file is not where that is resolved.
//!
//! Ported from `apps/web/src/lib/browseEntries.ts`. Pure string and array
//! math, so the picker's components stay wiring-only.

/// One name in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrowseEntry {
    /// The name, with no separator in it: a listing row is one component.
    pub name: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// Last modification, in milliseconds, or `0` when the machine did not say.
    pub mtime_ms: i64,
}

impl BrowseEntry {
    /// A directory row.
    #[must_use]
    pub fn dir(name: impl Into<String>, mtime_ms: i64) -> Self {
        Self {
            name: name.into(),
            is_dir: true,
            mtime_ms,
        }
    }

    /// A file row.
    #[must_use]
    pub fn file(name: impl Into<String>, mtime_ms: i64) -> Self {
        Self {
            name: name.into(),
            is_dir: false,
            mtime_ms,
        }
    }
}

/// The directories a filter admits, in name order.
#[must_use]
pub fn visible_folders(entries: &[BrowseEntry], filter: &str) -> Vec<BrowseEntry> {
    select_entries(entries, filter, true)
}

/// The files a filter admits, in name order.
#[must_use]
pub fn visible_files(entries: &[BrowseEntry], filter: &str) -> Vec<BrowseEntry> {
    select_entries(entries, filter, false)
}

/// Every directory's name, INCLUDING the dot-directories the list hides.
///
/// A sibling `mkdir` collides with a hidden name just as hard as with a shown
/// one, and the RPC that creates it is the only thing that knows whether the
/// name is taken. Filtering them out here is how "create folder" fails with a
/// name the list never showed.
#[must_use]
pub fn folder_names(entries: &[BrowseEntry]) -> Vec<String> {
    let mut names: Vec<String> = entries
        .iter()
        .filter(|entry| entry.is_dir)
        .map(|entry| entry.name.clone())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

fn select_entries(entries: &[BrowseEntry], filter: &str, want_dir: bool) -> Vec<BrowseEntry> {
    let needle = filter.trim().to_lowercase();
    let mut selected: Vec<BrowseEntry> = entries
        .iter()
        .filter(|entry| {
            if entry.is_dir != want_dir {
                return false;
            }
            // Dot-directories are hidden from the list, not from the machine.
            if entry.name.starts_with('.') {
                return false;
            }
            needle.is_empty() || entry.name.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    // Lower-cased byte order, NOT locale collation: v2 compared with
    // `localeCompare`, which puts `Ä` next to `A` in a German locale and after
    // `Z` in an English one, so the same listing was ordered differently on two
    // viewers. Byte order is the one order every viewer agrees on.
    selected.sort_by_key(|entry| entry.name.to_lowercase());
    selected
}

/// How an entry's modification time reads next to `now_ms`.
///
/// A machine that reports `0` — or a negative stamp, which is a machine whose
/// clock is behind — reads as no time at all rather than as fifty years ago.
#[must_use]
pub fn relative_entry_time(mtime_ms: i64, now_ms: i64) -> String {
    if mtime_ms <= 0 {
        return String::new();
    }
    let elapsed = now_ms.saturating_sub(mtime_ms);
    if elapsed < 60_000 {
        return "just now".to_owned();
    }
    if elapsed < 3_600_000 {
        return format!("{}m ago", elapsed / 60_000);
    }
    if elapsed < 86_400_000 {
        return format!("{}h ago", elapsed / 3_600_000);
    }
    if elapsed < 604_800_000 {
        return format!("{}d ago", elapsed / 86_400_000);
    }
    entry_date(mtime_ms)
}

/// The UTC calendar date a millisecond stamp falls on, as `YYYY-MM-DD`.
///
/// UTC and not the viewer's locale, because the viewer's locale is not knowable
/// here and two viewers in two locales must still agree on WHICH DAY a file
/// changed. A host that wants a localized rendering formats this date; it does
/// not re-derive the instant.
#[must_use]
pub fn entry_date(mtime_ms: i64) -> String {
    if mtime_ms <= 0 {
        return String::new();
    }
    let (year, month, day) = civil_from_days(mtime_ms.div_euclid(86_400_000));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The proleptic Gregorian date for a count of days since 1970-01-01.
///
/// Howard Hinnant's `civil_from_days`, which is exact across the whole range a
/// filesystem stamp can reach and needs no lookup table.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
