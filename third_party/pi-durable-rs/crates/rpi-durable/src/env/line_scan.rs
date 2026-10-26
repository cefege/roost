//! Bounded line scanning over streamed file bytes (port of
//! `packages/durable/src/env/line-scan.ts` @ `abe508e1b`).
//!
//! Computes a [`LineScan`] from a file's bytes fed in order, so an environment
//! can scan any size in bounded memory. Decoded sizes use the same streaming
//! decoder as decoding the whole file at once.

use thiserror::Error;

use crate::env::decode::{Utf8StreamDecoder, starts_with_bom};
use crate::env::types::LineScan;

const NEWLINE: u8 = 0x0a;

/// Rejected line range (upstream `RangeError("Invalid line range")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Invalid line range")]
pub struct InvalidLineRange;

/// Alias kept for callers that name the error by its scan role.
pub type LineScanError = InvalidLineRange;

/// Streaming line scanner; `start_line` and `end_line` are 0-based and
/// `end_line` is exclusive.
#[derive(Debug)]
pub struct LineScanner {
    start_line: u64,
    end_line: u64,
    position: u64,
    newlines: u64,
    line_start: u64,
    start: Option<u64>,
    end: Option<u64>,
    first_line_end: Option<u64>,
    last_line_start: Option<u64>,
    selected_bytes: u64,
    first_line_bytes: u64,
    selection: Option<Utf8StreamDecoder>,
    first_line: Option<Utf8StreamDecoder>,
    /// The first bytes, held until it is known whether they are a byte-order mark.
    head: Option<Vec<u8>>,
    bom: bool,
}

impl LineScanner {
    /// Create a scanner for `[start_line, end_line)`; `end_line` of `None`
    /// scans to the end of the file.
    pub fn new(start_line: u64, end_line: Option<u64>) -> Result<Self, InvalidLineRange> {
        let end_line = end_line.unwrap_or(u64::MAX);
        if end_line <= start_line {
            return Err(InvalidLineRange);
        }
        let start = (start_line == 0).then_some(0);
        Ok(Self {
            start_line,
            end_line,
            position: 0,
            newlines: 0,
            line_start: 0,
            start,
            end: None,
            first_line_end: None,
            last_line_start: None,
            selected_bytes: 0,
            first_line_bytes: 0,
            selection: start.map(|_| Utf8StreamDecoder::new()),
            first_line: start.map(|_| Utf8StreamDecoder::new()),
            head: Some(Vec::new()),
            bom: false,
        })
    }

    /// Feed the next chunk of the file.
    pub fn push(&mut self, chunk: &[u8]) {
        if let Some(head) = self.head.as_mut() {
            let take = (3 - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            if head.len() < 3 {
                return;
            }
            self.release_head();
            self.process(&chunk[take..]);
            return;
        }
        self.process(chunk);
    }

    fn release_head(&mut self) {
        let head = self.head.take().unwrap_or_default();
        self.bom = starts_with_bom(&head);
        self.process(&head);
    }

    fn process(&mut self, chunk: &[u8]) {
        let base = self.position;
        let mut from = 0usize;
        let mut search = 0usize;
        while let Some(offset) = chunk[search..].iter().position(|byte| *byte == NEWLINE) {
            let index = search + offset;
            // The newline ends line `self.newlines`; it belongs to the selection
            // between selected lines only.
            self.feed(chunk, base, from, index);
            let line = self.newlines;
            let position = base + index as u64;
            if line == self.start_line {
                self.end_first_line(position);
            }
            if line == self.end_line - 1 {
                self.end_selection(position);
            }
            self.feed(chunk, base, index, index + 1);
            from = index + 1;
            self.newlines += 1;
            self.line_start = position + 1;
            if self.newlines == self.start_line {
                self.begin(self.line_start);
            }
            if self.newlines == self.end_line - 1 {
                self.last_line_start = Some(self.line_start);
            }
            search = from;
        }
        self.feed(chunk, base, from, chunk.len());
        self.position += chunk.len() as u64;
    }

    /// Finish the scan; the same value decoding the whole file would produce.
    pub fn finish(&mut self) -> LineScan {
        if self.head.is_some() {
            self.release_head();
        }
        let size = self.position;
        let Some(start) = self.start else {
            return LineScan {
                newlines: self.newlines,
                start: size,
                end: size,
                first_line_end: size,
                last_line_start: size,
                selected_bytes: 0,
                first_line_bytes: 0,
            };
        };
        if self.first_line_end.is_none() {
            self.end_first_line(size);
        }
        if self.end.is_none() {
            self.end_selection(size);
        }
        LineScan {
            newlines: self.newlines,
            start,
            end: self.end.unwrap_or(size),
            first_line_end: self.first_line_end.unwrap_or(size),
            // A selection that reaches past the last line ends with the last line.
            last_line_start: self.last_line_start.unwrap_or(self.line_start),
            selected_bytes: self.selected_bytes,
            first_line_bytes: self.first_line_bytes,
        }
    }

    fn begin(&mut self, start: u64) {
        self.start = Some(start);
        if self.start_line == self.end_line - 1 {
            self.last_line_start = Some(start);
        }
        self.selection = Some(Utf8StreamDecoder::new());
        self.first_line = Some(Utf8StreamDecoder::new());
    }

    fn end_first_line(&mut self, position: u64) {
        self.first_line_end = Some(position);
        if let Some(first_line) = self.first_line.as_mut() {
            self.first_line_bytes += first_line.finish().len() as u64;
        }
        self.first_line = None;
    }

    fn end_selection(&mut self, position: u64) {
        self.end = Some(position);
        if let Some(selection) = self.selection.as_mut() {
            self.selected_bytes += selection.finish().len() as u64;
        }
        self.selection = None;
    }

    /// Feed `chunk[from..to]`, which starts at file offset `base`, to the
    /// decoders of the ranges still open.
    fn feed(&mut self, chunk: &[u8], base: u64, mut from: usize, to: usize) {
        // Decoding the whole file drops a leading byte-order mark.
        if self.bom && base + (from as u64) < 3 {
            from = to.min((3 - base) as usize);
        }
        if to <= from {
            return;
        }
        let bytes = &chunk[from..to];
        if let Some(selection) = self.selection.as_mut() {
            self.selected_bytes += selection.decode(bytes).len() as u64;
        }
        if let Some(first_line) = self.first_line.as_mut() {
            self.first_line_bytes += first_line.decode(bytes).len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the whole file like `new TextDecoder().decode(bytes)` (BOM dropped).
    fn decoded(bytes: &[u8]) -> String {
        let mut decoder = Utf8StreamDecoder::new();
        let mut text = decoder.decode(bytes);
        text.push_str(&decoder.finish());
        if text.starts_with('\u{feff}') {
            text.remove(0);
        }
        text
    }

    fn scan(bytes: &[u8], start: u64, end: Option<u64>) -> LineScan {
        let mut scanner = LineScanner::new(start, end).expect("range");
        // Feed in small chunks to exercise the streaming paths.
        for chunk in bytes.chunks(2) {
            scanner.push(chunk);
        }
        scanner.finish()
    }

    #[test]
    fn scans_like_slicing_the_decoded_lines() {
        let bytes: Vec<u8> = [
            0xef, 0xbb, 0xbf, 0x61, 0x0a, 0xe2, 0x82, 0x0a, 0x0a, 0xef, 0xbb, 0xbf, 0x62, 0x0a,
            0xc3, 0xa9,
        ]
        .to_vec();
        let text = decoded(&bytes);
        let lines: Vec<&str> = text.split('\n').collect();
        for (start, end) in [
            (0u64, None),
            (0, Some(1)),
            (1, Some(3)),
            (2, Some(3)),
            (3, None),
            (4, Some(9)),
        ] {
            let result = scan(&bytes, start, end);
            let selected = match end {
                Some(end) => lines
                    .get(start as usize..(end as usize).min(lines.len()))
                    .unwrap_or_default(),
                None => lines.get(start as usize..).unwrap_or_default(),
            };
            assert_eq!(result.newlines, lines.len() as u64 - 1);
            let range = |from: u64, to: u64| {
                let slice = &bytes[from as usize..to as usize];
                let mut decoder = Utf8StreamDecoder::new();
                let mut text = decoder.decode(slice);
                text.push_str(&decoder.finish());
                if from == 0 && text.starts_with('\u{feff}') {
                    text.remove(0);
                }
                text
            };
            assert_eq!(
                range(result.start, result.end),
                selected.join("\n"),
                "range {start}..{end:?}"
            );
            assert_eq!(result.selected_bytes, selected.join("\n").len() as u64);
            if let Some(first) = lines.get(start as usize) {
                assert_eq!(range(result.start, result.first_line_end), *first);
                assert_eq!(result.first_line_bytes, first.len() as u64);
            }
        }
        let past = scan(&bytes, 9, None);
        assert_eq!(past.start, bytes.len() as u64);
        assert_eq!(past.end, bytes.len() as u64);
        assert_eq!(past.selected_bytes, 0);
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert!(LineScanner::new(2, Some(2)).is_err());
        assert!(LineScanner::new(3, Some(2)).is_err());
        assert!(LineScanner::new(0, Some(1)).is_ok());
        assert!(LineScanner::new(0, None).is_ok());
    }

    #[test]
    fn empty_file_scans_empty() {
        let result = scan(b"", 0, None);
        assert_eq!(result.newlines, 0);
        assert_eq!(result.start, 0);
        assert_eq!(result.end, 0);
        assert_eq!(result.selected_bytes, 0);
        assert_eq!(result.first_line_bytes, 0);
    }

    #[test]
    fn selection_past_the_last_line_ends_at_the_file_end() {
        let result = scan(b"a\nb", 5, None);
        assert_eq!(result.start, 3);
        assert_eq!(result.end, 3);
        assert_eq!(result.selected_bytes, 0);
    }
}
