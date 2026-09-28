//! The filename an upload lands under: every control character stripped, the
//! leaf kept, its extension preserved, the stem bounded and never a dotfile.
//! Ports the POSIX branch of v2 `sanitizeAttachmentName`
//! (`apps/worker/src/attachments/attachment-reaper.ts`) together with the
//! Node `path.posix.extname` rule it parses with. Called by the file store.

/// A sanitized name's budget, in UTF-16 code units because v2 measured and
/// sliced JavaScript strings.
const NAME_MAX_UNITS: usize = 80;

/// What a stem that sanitizes to nothing becomes.
const EMPTY_STEM: &str = "file";

/// v2 `sanitizeAttachmentName` on darwin and linux.
pub fn sanitize_attachment_name(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|character| !is_stripped_control(*character))
        .collect();
    let leaf = last_segment(&clean);
    let extension = node_extname(leaf);
    let stem_max = NAME_MAX_UNITS
        .saturating_sub(extension.encode_utf16().count())
        .max(1);
    let stem_source = &leaf[..leaf.len() - extension.len()];
    let stem = truncate_utf16(stem_source.trim_start_matches('.'), stem_max);
    if stem.is_empty() {
        return format!("{EMPTY_STEM}{extension}");
    }
    format!("{stem}{extension}")
}

/// Node `path.posix.extname` of one leaf: from its last dot, unless that dot
/// begins the leaf (a dotfile has no extension) or the leaf is exactly `..`.
pub(crate) fn node_extname(leaf: &str) -> &str {
    let bytes = leaf.as_bytes();
    let Some(last_dot) = bytes.iter().rposition(|byte| *byte == b'.') else {
        return "";
    };
    if last_dot == 0 || leaf == ".." {
        return "";
    }
    &leaf[last_dot..]
}

/// C0, DEL and C1 — v2's `/[\x00-\x1F\x7F-\x9F]/g`.
fn is_stripped_control(character: char) -> bool {
    character <= '\u{1F}' || ('\u{7F}'..='\u{9F}').contains(&character)
}

/// The last `/`-separated segment, trailing separators ignored, as Node's
/// `basename` reads a path.
fn last_segment(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// JavaScript's `slice(0, max_units)`. A surrogate pair cut in half leaves a
/// lone surrogate, which reaches the filesystem as U+FFFD.
fn truncate_utf16(text: &str, max_units: usize) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut used = 0;
    for character in text.chars() {
        let units = character.len_utf16();
        if used + units > max_units {
            if used < max_units {
                kept.push('\u{FFFD}');
            }
            break;
        }
        used += units;
        kept.push(character);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_c0_del_and_c1_control_is_removed_before_the_name_is_parsed() {
        let controls: String = (0u32..0x20)
            .chain(0x7f..=0x9f)
            .filter_map(char::from_u32)
            .collect();
        let inert = "safe name ' $() `tick` 雪";
        let raw = format!("ignored/{controls}{inert}{controls}.tar{controls}.gz{controls}");
        assert_eq!(sanitize_attachment_name(&raw), format!("{inert}.tar.gz"));
    }

    #[test]
    fn inert_spaces_quotes_shell_syntax_and_unicode_survive() {
        let filename = "ordinary name ' \" $() `backticks` 文.txt";
        assert_eq!(sanitize_attachment_name(filename), filename);
    }

    #[test]
    fn a_dotfile_loses_its_leading_dots_and_an_empty_stem_becomes_file() {
        assert_eq!(sanitize_attachment_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_attachment_name("..a.txt"), "a.txt");
        assert_eq!(sanitize_attachment_name(".txt"), "txt");
        assert_eq!(sanitize_attachment_name(""), "file");
        assert_eq!(sanitize_attachment_name("dir/"), "dir");
        assert_eq!(sanitize_attachment_name("..."), "file.");
        assert_eq!(sanitize_attachment_name(".."), "file");
        assert_eq!(sanitize_attachment_name("ignored/a.tar.gz"), "a.tar.gz");
    }

    #[test]
    fn the_stem_is_bounded_in_utf16_units_and_the_extension_is_kept() {
        let long = format!("{}.markdown", "a".repeat(200));
        assert_eq!(
            sanitize_attachment_name(&long),
            format!("{}.markdown", "a".repeat(71))
        );
        let wide = format!("{}.x", "😀".repeat(50));
        assert_eq!(
            sanitize_attachment_name(&wide),
            format!("{}.x", "😀".repeat(39))
        );
        let split = format!("{}😀.y", "x".repeat(77));
        assert_eq!(
            sanitize_attachment_name(&split),
            format!("{}\u{FFFD}.y", "x".repeat(77)),
            "half a surrogate pair survives JavaScript's slice"
        );
    }
}
