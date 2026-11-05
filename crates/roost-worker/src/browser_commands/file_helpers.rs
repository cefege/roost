//! Byte-preserving helpers shared by filesystem reads.
//!
//! The file command module uses these for bounded previews and contextual I/O errors.

pub(super) fn head_lines(bytes: &[u8], limit: i64) -> Vec<u8> {
    let limit = usize::try_from(limit).unwrap_or(0);
    let mut end = 0usize;
    for line in String::from_utf8_lossy(bytes)
        .split_inclusive('\n')
        .take(limit)
    {
        end += line.len();
    }
    let mut end = end.min(bytes.len());
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    bytes[..end].to_vec()
}

pub(super) fn describe(path: &str, error: &std::io::Error) -> String {
    format!("{path}: {error}")
}
