//! Streaming UTF-8 decoding (port of `packages/durable/src/env/decode.ts`
//! @ `abe508e1b`).
//!
//! Decoding matches `new TextDecoder().decode(bytes)` of the whole input when
//! the input arrives in pieces. Node's streaming decoder with BOM handling can
//! drop a U+FEFF that follows a chunk boundary, not only a leading byte-order
//! mark, so the port decodes without BOM handling and drops a leading mark
//! itself.
//!
//! Replacement semantics follow the WHATWG decoder: one U+FFFD per maximal
//! subpart of an ill-formed subsequence (the same rule as Rust's
//! `String::from_utf8_lossy`), with an incomplete trailing sequence held back
//! until the next chunk.

/// Whether the first bytes are a UTF-8 byte-order mark.
pub fn starts_with_bom(first_bytes: &[u8]) -> bool {
    first_bytes.len() >= 3
        && first_bytes[0] == 0xef
        && first_bytes[1] == 0xbb
        && first_bytes[2] == 0xbf
}

/// A fresh streaming decoder for a byte range; callers that start at the
/// beginning of a file skip a leading mark themselves (`rangeDecoder`).
pub fn range_decoder() -> Utf8StreamDecoder {
    Utf8StreamDecoder::new()
}

/// Decodes byte chunks exactly like decoding all of them at once, without BOM
/// handling.
#[derive(Debug, Default)]
pub struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// A fresh decoder.
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Text for `bytes`, holding back an incomplete character.
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    return text;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    // SAFETY: `valid_up_to` marks the end of the valid prefix.
                    text.push_str(unsafe {
                        std::str::from_utf8_unchecked(&self.pending[..valid_up_to])
                    });
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{fffd}');
                            self.pending.drain(..valid_up_to + length);
                        }
                        None => {
                            self.pending.drain(..valid_up_to);
                            return text;
                        }
                    }
                }
            }
        }
    }

    /// Text for the end of the stream: an incomplete trailing character
    /// becomes a replacement character.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

/// Decodes one stream chunk by chunk exactly like decoding all of it at once,
/// dropping only a byte-order mark at the very start of the output
/// (`StreamDecoder`).
#[derive(Debug, Default)]
pub struct StreamDecoder {
    decoder: Utf8StreamDecoder,
    started: bool,
}

impl StreamDecoder {
    /// A fresh decoder.
    pub fn new() -> Self {
        Self {
            decoder: Utf8StreamDecoder::new(),
            started: false,
        }
    }

    /// Text for `bytes`, holding back an incomplete character; for the end of
    /// the stream call [`Self::finish`].
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        let text = self.decoder.decode(bytes);
        self.strip_bom(text)
    }

    /// Text for the end of the stream.
    pub fn finish(&mut self) -> String {
        let text = self.decoder.finish();
        self.strip_bom(text)
    }

    fn strip_bom(&mut self, text: String) -> String {
        if self.started || text.is_empty() {
            return text;
        }
        self.started = true;
        // U+FEFF encodes only as EF BB BF, so a leading U+FEFF is exactly a
        // byte-order mark at the stream's start.
        text.strip_prefix('\u{feff}')
            .map(str::to_string)
            .unwrap_or(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_byte_order_marks() {
        assert!(starts_with_bom(&[0xef, 0xbb, 0xbf]));
        assert!(!starts_with_bom(&[0xef, 0xbb]));
        assert!(!starts_with_bom(b"abc"));
    }

    #[test]
    fn stream_decoder_drops_only_the_leading_mark() {
        let mut decoder = StreamDecoder::new();
        let bom = [0xef, 0xbb, 0xbf];
        assert_eq!(decoder.decode(&bom), "");
        assert_eq!(decoder.decode(b"ab"), "ab");
        let mut later = decoder.decode(&bom);
        later.push('c');
        assert_eq!(later, "\u{feff}c");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn holds_back_split_characters() {
        let mut decoder = Utf8StreamDecoder::new();
        let euro = "€".as_bytes();
        assert_eq!(decoder.decode(&euro[..1]), "");
        assert_eq!(decoder.decode(&euro[1..]), "€");
        assert_eq!(decoder.decode(b"x"), "x");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn replaces_invalid_sequences_by_maximal_subpart() {
        let mut decoder = Utf8StreamDecoder::new();
        // A truncated sequence before a newline: one replacement, then the newline.
        assert_eq!(decoder.decode(&[0xe2, 0x82, 0x0a]), "\u{fffd}\n");
        // A lone continuation byte is one replacement.
        assert_eq!(decoder.decode(&[0x80, b'a']), "\u{fffd}a");
    }

    #[test]
    fn flushes_an_incomplete_trailing_character() {
        let mut decoder = Utf8StreamDecoder::new();
        let euro = "€".as_bytes();
        assert_eq!(decoder.decode(&euro[..1]), "");
        assert_eq!(decoder.finish(), "\u{fffd}");
    }
}
