//! Image MIME detection (port of `packages/durable/src/tools/image.ts`
//! @ `abe508e1b`).
//!
//! The durable version detects supported image types only; image content reads
//! belong to the post-v1.1.0 overflow and are out of this window.

use async_trait::async_trait;

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
/// Bytes every check except the APNG chunk walk needs: BMP reads up to offset 29.
const HEADER_BYTES: u64 = 32;
const BLOCK_BYTES: u64 = 64 * 1024;

/// Positional reads of a file of `size` bytes (`ByteSource`).
#[async_trait]
pub trait ByteSource: Send + Sync {
    /// File size in bytes.
    fn size(&self) -> u64;
    /// Bytes at `offset`.
    async fn read(&self, offset: u64, length: u64) -> std::io::Result<Vec<u8>>;
}

/// [`detect_supported_image_mime_type`] of a whole file, reading only its
/// header and, for PNG, the chunk headers up to the first `acTL` or `IDAT`.
pub async fn detect_supported_image_mime_type_of(
    source: &dyn ByteSource,
) -> std::io::Result<Option<String>> {
    let header = source.read(0, HEADER_BYTES).await?;
    if !starts_with(&header, &PNG_SIGNATURE) {
        return Ok(detect_supported_image_mime_type(&header).map(str::to_string));
    }
    if is_png(&header) && !is_animated_png_of(source).await? {
        Ok(Some("image/png".to_string()))
    } else {
        Ok(None)
    }
}

/// `isAnimatedPng` over a file read in blocks.
async fn is_animated_png_of(source: &dyn ByteSource) -> std::io::Result<bool> {
    let mut block: Vec<u8> = Vec::new();
    let mut block_start: u64 = 0;
    let mut offset = PNG_SIGNATURE.len() as u64;
    while offset + 8 <= source.size() {
        let chunk_header = if offset < block_start || offset + 8 > block_start + block.len() as u64
        {
            block_start = offset;
            block = source.read(offset, BLOCK_BYTES).await?;
            block.clone()
        } else {
            block[(offset - block_start) as usize..(offset - block_start) as usize + 8].to_vec()
        };
        let length = read_uint32_be(&chunk_header, 0);
        if starts_with_ascii(&chunk_header, 4, "acTL") {
            return Ok(true);
        }
        if starts_with_ascii(&chunk_header, 4, "IDAT") {
            return Ok(false);
        }
        let next = offset + 8 + length as u64 + 4;
        if next <= offset || next > source.size() {
            return Ok(false);
        }
        offset = next;
    }
    Ok(false)
}

/// Detect a supported image MIME type from a buffer (upstream
/// `detectSupportedImageMimeType`).
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
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

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_uint32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_uint32_be(buffer, offset);
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let next = offset + 8 + chunk_length as usize + 4;
        if next <= offset || next > buffer.len() {
            return false;
        }
        offset = next;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_uint32_le(buffer, 2);
    let pixel_data_offset = read_uint32_le(buffer, 10);
    let dib_header_size = read_uint32_le(buffer, 14);
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
        (read_uint16_le(buffer, 22), read_uint16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_uint16_le(buffer, 26), read_uint16_le(buffer, 28))
    } else {
        return false;
    };
    color_planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits_per_pixel)
}

fn read_uint16_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from(buffer.get(offset).copied().unwrap_or(0))
        + (u32::from(buffer.get(offset + 1).copied().unwrap_or(0)) << 8)
}

fn read_uint32_be(buffer: &[u8], offset: usize) -> u32 {
    u32::from(buffer.get(offset).copied().unwrap_or(0)) * 0x100_0000
        + (u32::from(buffer.get(offset + 1).copied().unwrap_or(0)) << 16)
        + (u32::from(buffer.get(offset + 2).copied().unwrap_or(0)) << 8)
        + u32::from(buffer.get(offset + 3).copied().unwrap_or(0))
}

fn read_uint32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from(buffer.get(offset).copied().unwrap_or(0))
        + (u32::from(buffer.get(offset + 1).copied().unwrap_or(0)) << 8)
        + (u32::from(buffer.get(offset + 2).copied().unwrap_or(0)) << 16)
        + u32::from(buffer.get(offset + 3).copied().unwrap_or(0)) * 0x100_0000
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && &buffer[..bytes.len()] == bytes
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    buffer.len() >= offset + text.len() && &buffer[offset..offset + text.len()] == text.as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_formats() {
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xf7]),
            None
        );
        assert_eq!(
            detect_supported_image_mime_type(b"GIF89a----"),
            Some("image/gif")
        );
        assert_eq!(
            detect_supported_image_mime_type(b"RIFFxxxxWEBP"),
            Some("image/webp")
        );
        assert_eq!(detect_supported_image_mime_type(b"not an image"), None);
    }

    #[test]
    fn refuses_animated_pngs() {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0; 13]);
        png.extend_from_slice(&[0; 4]); // IHDR CRC
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"acTL");
        assert_eq!(detect_supported_image_mime_type(&png), None);
    }
}
