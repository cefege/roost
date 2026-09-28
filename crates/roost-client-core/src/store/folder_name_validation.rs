//! The folder picker's "New folder" name check, so an invalid name is named
//! inline before any mkdir leaves the browser; the first failing rule wins and
//! its message is what the dialog shows. Ports
//! `apps/web/src/lib/folderNameValidation.ts`; the browse picker calls it.

/// The longest name, in UTF-16 code units as v2 measured it.
pub const FOLDER_NAME_MAX: usize = 255;

/// Why a name is refused, as the dialog words it; `Ok(())` when it is fine.
pub fn validate_new_folder_name(name: &str, sibling_names: &[&str]) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Enter a folder name.".to_owned());
    }
    if name.contains(['/', '\\']) {
        return Err("Folder names can't contain / or \\.".to_owned());
    }
    if name.chars().any(|character| u32::from(character) < 0x20) {
        return Err("Folder names can't contain control characters.".to_owned());
    }
    // Before the trailing-period rule, so `..` names the dot rule rather than
    // the less specific one.
    if trimmed == "." || trimmed == ".." {
        return Err("Choose a name other than . or ..".to_owned());
    }
    if trimmed.ends_with([' ', '.']) {
        return Err("Folder names can't end with a space or period.".to_owned());
    }
    if trimmed.encode_utf16().count() > FOLDER_NAME_MAX {
        return Err(format!("Folder names must be {FOLDER_NAME_MAX} characters or fewer."));
    }
    let lower = trimmed.to_lowercase();
    if sibling_names.iter().any(|sibling| sibling.to_lowercase() == lower) {
        return Err(format!("A folder named \"{trimmed}\" already exists here."));
    }
    Ok(())
}
