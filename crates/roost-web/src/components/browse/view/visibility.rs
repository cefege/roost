//! Picker entry visibility over a borrowed worker listing.
//! Called by `view::read_picker`; store filters own ordinary entries.
//! Hidden entries join the same projection without copying the raw listing.

use roost_client_core::store::browse_entries::{BrowseEntry, visible_files, visible_folders};

pub(super) fn visible_entries(
    entries: &[BrowseEntry],
    filter: &str,
    show_hidden: bool,
) -> (Vec<BrowseEntry>, Vec<BrowseEntry>) {
    let mut folders = visible_folders(entries, filter);
    let mut files = visible_files(entries, filter);
    if show_hidden {
        let needle = filter.trim().to_lowercase();
        for entry in entries {
            if entry.name.starts_with('.') && entry.name.to_lowercase().contains(&needle) {
                if entry.is_dir {
                    folders.push(entry.clone());
                } else {
                    files.push(entry.clone());
                }
            }
        }
        folders.sort_by_cached_key(|entry| entry.name.to_lowercase());
        files.sort_by_cached_key(|entry| entry.name.to_lowercase());
    }
    (folders, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_files_and_folders_share_the_filter_and_toggle() {
        let entries = [
            BrowseEntry::dir(".Config", 1),
            BrowseEntry::file(".config.json", 1),
            BrowseEntry::dir("other", 1),
        ];
        let (folders, files) = visible_entries(&entries, "CONFIG", false);
        assert!(folders.is_empty());
        assert!(files.is_empty());

        let (folders, files) = visible_entries(&entries, "CONFIG", true);
        assert_eq!(
            folders
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [".Config"]
        );
        assert_eq!(
            files
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [".config.json"]
        );
    }
}
