//! Port of `packages/agent/src/harness/tools/image.ts` @ pi 0.82.1 (2efa728) —
//! image MIME detection from magic bytes and base64 encoding for the read
//! tool's image attachments.
//!
//! Intentional differences:
//! - `encodeBase64` (image.ts:12-25) is the `base64` crate's
//!   `general_purpose::STANDARD` encoder — byte-identical output (standard
//!   alphabet, `=` padding).
//! - The `?? 0` out-of-bounds reads of the `readUint*` helpers (image.ts:71-91)
//!   are `get().copied().unwrap_or(0)`, so a truncated buffer reads zeros
//!   exactly like JS.
//! - `startsWith` / `startsWithAscii` become byte comparisons on `&[u8]`.

/// `PNG_SIGNATURE` (image.ts:1).
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// `detectSupportedImageMimeType` (image.ts:3-10) — sniff the magic bytes for
/// jpg / png / gif / webp / bmp. Animated PNGs (`acTL` chunk before `IDAT`) and
/// JPEGs with the `0xF7` "start of image" extension marker are rejected.
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        // `buffer[3] === 0xf7` — an out-of-bounds read yields `undefined`,
        // which is not `0xf7` (image.ts:4).
        return if buffer.get(3) == Some(&0xf7) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png")
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF87a") || starts_with_ascii(buffer, 0, "GIF89a") {
        return Some("image/gif");
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

/// `encodeBase64` (image.ts:12-25).
pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `isPng` (image.ts:27-31): 8-byte signature, 13-byte first chunk, `IHDR`.
fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

/// `isAnimatedPng` (image.ts:33-45): scan the chunk list; an `acTL` chunk
/// before the first `IDAT` marks an animated PNG.
fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset);
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let next_offset = offset + 8 + chunk_length as usize + 4;
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

/// `isBmp` (image.ts:47-69).
fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_u32_le(buffer, 2);
    let pixel_data_offset = read_u32_le(buffer, 10);
    let dib_header_size = read_u32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };
    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

/// `readUint16LE` (image.ts:71-73).
fn read_u16_le(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32)
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 8)
}

/// `readUint32BE` (image.ts:75-82).
fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32) * 0x0100_0000
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 16)
        + ((buffer.get(offset + 2).copied().unwrap_or(0) as u32) << 8)
        + buffer.get(offset + 3).copied().unwrap_or(0) as u32
}

/// `readUint32LE` (image.ts:84-91).
fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32)
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 8)
        + ((buffer.get(offset + 2).copied().unwrap_or(0) as u32) << 16)
        + (buffer.get(offset + 3).copied().unwrap_or(0) as u32) * 0x0100_0000
}

/// `startsWith` (image.ts:93-96).
fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && buffer[..bytes.len()] == *bytes
}

/// `startsWithAscii` (image.ts:98-103).
fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    let end = offset.saturating_add(text.len());
    if end > buffer.len() {
        return false;
    }
    buffer[offset..end]
        .iter()
        .zip(text.bytes())
        .all(|(a, b)| *a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::tools::test_helpers::{tiny_bmp, tiny_png};

    #[test]
    fn detects_png_by_signature() {
        assert_eq!(
            detect_supported_image_mime_type(&tiny_png()),
            Some("image/png")
        );
    }

    #[test]
    fn detects_bmp_by_structure() {
        assert_eq!(
            detect_supported_image_mime_type(&tiny_bmp()),
            Some("image/bmp")
        );
    }

    #[test]
    fn detects_jpeg_gif_webp() {
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        // `buffer[3] === 0xf7` → rejected (image.ts:4).
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xf7]),
            None
        );
        assert_eq!(
            detect_supported_image_mime_type(b"GIF89a"),
            Some("image/gif")
        );
        // #9755 (tools.test.ts @ 47a18e37b): both complete signatures.
        assert_eq!(
            detect_supported_image_mime_type(b"GIF87a"),
            Some("image/gif")
        );
        assert_eq!(
            detect_supported_image_mime_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
    }

    #[test]
    fn bare_gif_prefix_is_not_an_image() {
        assert_eq!(detect_supported_image_mime_type(b"GIF"), None);
        assert_eq!(
            detect_supported_image_mime_type(b"GIF image description"),
            None
        );
    }

    #[test]
    fn rejects_non_images_and_short_buffers() {
        assert_eq!(detect_supported_image_mime_type(b""), None);
        assert_eq!(detect_supported_image_mime_type(b"plain text"), None);
        // Truncated JPEG signature (3 bytes) still matches (JS reads
        // `buffer[3]` as undefined, which is not 0xf7).
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff]),
            Some("image/jpeg")
        );
        assert_eq!(detect_supported_image_mime_type(b"BM"), None);
        assert_eq!(
            detect_supported_image_mime_type(b"BM012345678901234567890"),
            None
        );
    }

    #[test]
    fn rejects_animated_png() {
        // Craft a minimal stream sig + IHDR + acTL + IDAT by hand.
        let mut bytes: Vec<u8> = PNG_SIGNATURE.to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&[0u8; 13]);
        bytes.extend_from_slice(&[0, 0, 0, 0]); // CRC
        bytes.extend_from_slice(&[0, 0, 0, 4]); // acTL length
        bytes.extend_from_slice(b"acTL");
        bytes.extend_from_slice(&[0, 0, 0, 0]); // CRC
        bytes.extend_from_slice(&[0, 0, 0, 8]); // IDAT length
        bytes.extend_from_slice(b"IDAT");
        bytes.extend_from_slice(&[0u8; 8]);
        bytes.extend_from_slice(&[0, 0, 0, 0]); // CRC
        assert!(is_png(&bytes));
        assert!(is_animated_png(&bytes));
        assert_eq!(detect_supported_image_mime_type(&bytes), None);
        // Sanity: the same stream without acTL is a plain (supported) PNG.
        let mut plain = PNG_SIGNATURE.to_vec();
        plain.extend_from_slice(&[0, 0, 0, 13]);
        plain.extend_from_slice(b"IHDR");
        plain.extend_from_slice(&[0u8; 17]);
        plain.extend_from_slice(&[0, 0, 0, 8]);
        plain.extend_from_slice(b"IDAT");
        plain.extend_from_slice(&[0u8; 8]);
        plain.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(detect_supported_image_mime_type(&plain), Some("image/png"));
    }

    #[test]
    fn encode_base64_matches_standard_alphabet() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode_base64(&[0xfb, 0xff, 0x00, 0x01]), "+/8AAQ==");
    }
}
