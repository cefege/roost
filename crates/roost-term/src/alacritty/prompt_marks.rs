//! OSC 133 prompt markers in the byte stream.
//!
//! The terminal parser discards unknown OSC commands. This scanner records only
//! completed 133 commands, carries a bounded partial command across PTY chunks,
//! and lets the adapter stop parsing at the cursor position where each marker
//! occurred.

/// A grid action encoded by one complete OSC 133 command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptMark {
    Prompt,
    Output,
    Finished(bool),
}

/// One command's position after its BEL or ST terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkAt {
    pub end: usize,
    pub mark: PromptMark,
}
const PREFIX: &[u8; 6] = b"\x1b]133;";
const MAX_BODY_BYTES: usize = 64;

/// Bounded OSC 133 recognizer. It retains only the command and status bytes
/// needed to recognize a marker, consuming the rest of its parameters in place.
pub(super) struct PromptMarkScanner {
    prefix: [u8; 6],
    prefix_len: usize,
    in_body: bool,
    body: [u8; MAX_BODY_BYTES],
    body_len: usize,
    escape: bool,
}

impl Default for PromptMarkScanner {
    fn default() -> Self {
        Self {
            prefix: [0; 6],
            prefix_len: 0,
            in_body: false,
            body: [0; MAX_BODY_BYTES],
            body_len: 0,
            escape: false,
        }
    }
}

impl PromptMarkScanner {
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<MarkAt> {
        if !self.in_body && self.prefix_len == 0 && !bytes.contains(&0x1b) {
            return Vec::new();
        }
        let mut marks = Vec::new();
        for (idx, byte) in bytes.iter().copied().enumerate() {
            if self.in_body {
                if self.escape {
                    self.escape = false;
                    if byte == b'\\' {
                        self.finish(idx + 1, &mut marks);
                    } else {
                        self.push_body(0x1b);
                        self.push_body(byte);
                    }
                } else if byte == 0x1b {
                    self.escape = true;
                } else if byte == 0x07 {
                    self.finish(idx + 1, &mut marks);
                } else {
                    self.push_body(byte);
                }
                continue;
            }

            if byte == PREFIX[self.prefix_len] {
                self.prefix[self.prefix_len] = byte;
                self.prefix_len += 1;
                if self.prefix_len == PREFIX.len() {
                    self.prefix_len = 0;
                    self.in_body = true;
                    self.body_len = 0;
                    self.escape = false;
                }
            } else if byte == PREFIX[0] {
                self.prefix[0] = byte;
                self.prefix_len = 1;
            } else {
                self.prefix_len = 0;
            }
        }
        marks
    }

    fn push_body(&mut self, byte: u8) {
        if self.body_len < self.body.len() {
            self.body[self.body_len] = byte;
            self.body_len += 1;
        }
    }

    fn finish(&mut self, end: usize, marks: &mut Vec<MarkAt>) {
        if let Some(mark) = parse_body(&self.body[..self.body_len]) {
            marks.push(MarkAt { end, mark });
        }
        self.in_body = false;
        self.body_len = 0;
        self.escape = false;
    }
}

fn parse_body(body: &[u8]) -> Option<PromptMark> {
    match body.first().copied()? {
        b'A' => Some(PromptMark::Prompt),
        b'C' => Some(PromptMark::Output),
        b'D' => {
            let status = body
                .strip_prefix(b"D;")
                .and_then(|value| value.split(|byte| *byte == b';').next())
                .filter(|digits| !digits.is_empty())
                .map(|digits| {
                    digits
                        .iter()
                        .try_fold(0u32, |value, digit| {
                            digit.is_ascii_digit().then(|| {
                                value
                                    .saturating_mul(10)
                                    .saturating_add(u32::from(*digit - b'0'))
                            })
                        })
                        .unwrap_or(u32::MAX)
                })
                .unwrap_or(0);
            Some(PromptMark::Finished(status == 0))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MarkAt, PromptMark, PromptMarkScanner};

    #[test]
    fn complete_marks_report_terminator_offsets_and_ignore_other_commands() {
        let mut scanner = PromptMarkScanner::default();
        let bytes = b"\x1b]133;A\x07x\x1b]133;C;key=value\x1b\\\x1b]133;D;7\x07";
        assert_eq!(
            scanner.scan(bytes),
            vec![
                MarkAt {
                    end: 8,
                    mark: PromptMark::Prompt
                },
                MarkAt {
                    end: 28,
                    mark: PromptMark::Output
                },
                MarkAt {
                    end: 38,
                    mark: PromptMark::Finished(false)
                },
            ]
        );
    }

    #[test]
    fn marker_prefix_and_st_terminator_survive_chunk_boundaries() {
        let mut scanner = PromptMarkScanner::default();
        assert!(scanner.scan(b"before\x1b]133").is_empty());
        assert_eq!(scanner.scan(b";D;0\x1b"), Vec::<MarkAt>::new());
        assert_eq!(
            scanner.scan(b"\\after"),
            vec![MarkAt {
                end: 1,
                mark: PromptMark::Finished(true)
            }]
        );
    }
}
