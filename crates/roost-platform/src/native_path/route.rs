//! The browse route codec: a path in a URL, and back. POSIX encoding is
//! byte-for-byte the historical route codec — percent-encoded segments with
//! the leading slash stripped — so an old bookmark still resolves. Windows
//! needs more than a percent-encoded path: a drive letter and a UNC root both
//! look like ordinary segments, so the route carries a tagged root
//! (`~drive/C/…`, `~unc/host/share/…`) and the decoder refuses anything that
//! does not name one.

use crate::host_platform::HostPlatform;
use crate::native_path::normalize::{
    NativePathError, normalize_native_path, reject_invalid_path, split_drive_anchor,
};

const UPPERCASE_HEX: [u8; 16] = *b"0123456789ABCDEF";

/// The characters `encodeURIComponent` leaves alone, on top of alphanumerics.
/// Notably `'` and `!~*()` survive, so a POSIX file name with a quote in it
/// is not mangled by a trip through a route.
const UNRESERVED_PUNCTUATION: [u8; 9] = *b"-_.!~*'()";

/// Encode a canonical-looking path as a route. POSIX encoding deliberately
/// does NOT normalize: a route is what the caller linked to, and folding
/// `..` here would make two different routes one.
pub fn encode_native_path_route(
    platform: HostPlatform,
    path: &str,
) -> Result<String, NativePathError> {
    reject_invalid_path(path)?;
    match platform {
        HostPlatform::MacOs | HostPlatform::Linux => {
            if !path.starts_with('/') {
                return Err(NativePathError::PosixNotAbsolute(path.to_owned()));
            }
            let encoded: Vec<String> = path
                .split('/')
                .map(|segment| {
                    if segment.is_empty() {
                        String::new()
                    } else {
                        encode_uri_component(segment)
                    }
                })
                .collect();
            let route = encoded.join("/");
            Ok(route.strip_prefix('/').unwrap_or(&route).to_owned())
        }
        HostPlatform::Windows => {
            let normalized = normalize_native_path(platform, path)?;
            if normalized == "~" || normalized.starts_with("~/") {
                return Err(NativePathError::HomeSentinelUnresolvedForRoute);
            }
            if let Some((drive_letter, tail)) = split_drive_anchor(&normalized) {
                let encoded_tail = encode_segments(tail);
                let route = format!("~drive/{drive_letter}/{encoded_tail}");
                return Ok(route.strip_suffix('/').map(str::to_owned).unwrap_or(route));
            }
            let fields = normalized[2..].replacen('\\', "/", usize::MAX);
            Ok(format!("~unc/{}", encode_segments(&fields)))
        }
    }
}

/// Decode a route back to a path. A segment that decodes to something empty,
/// or to a separator or a NUL, is refused: it is a route that would re-split
/// itself, and a path that means two things is a path two machines disagree
/// about.
pub fn decode_native_path_route(
    platform: HostPlatform,
    route: &str,
) -> Result<String, NativePathError> {
    match platform {
        HostPlatform::MacOs | HostPlatform::Linux => {
            let mut decoded_segments = Vec::new();
            for segment in route.split('/') {
                if segment.is_empty() {
                    decoded_segments.push(String::new());
                } else {
                    decoded_segments.push(decode_uri_component(segment)?);
                }
            }
            let inner = decoded_segments.join("/");
            let inner = inner.strip_prefix('/').unwrap_or(&inner);
            let decoded = format!("/{inner}");
            reject_invalid_path(&decoded)?;
            Ok(decoded)
        }
        HostPlatform::Windows => {
            let mut parts = decode_segments(route)?;
            let Some(tag) = parts.first().cloned() else {
                return Err(NativePathError::MissingTaggedRoot);
            };
            parts.remove(0);
            match tag.as_str() {
                "~drive" => {
                    let Some(drive) = parts.first().cloned() else {
                        return Err(NativePathError::InvalidDriveRoute);
                    };
                    if !is_single_ascii_letter(&drive) {
                        return Err(NativePathError::InvalidDriveRoute);
                    }
                    parts.remove(0);
                    normalize_native_path(platform, &format!("{drive}:/{}", parts.join("/")))
                }
                "~unc" => {
                    if parts.len() < 2 {
                        return Err(NativePathError::InvalidUncRoute);
                    }
                    normalize_native_path(platform, &format!("//{}", parts.join("/")))
                }
                _ => Err(NativePathError::MissingTaggedRoot),
            }
        }
    }
}

fn encode_segments(segments: &str) -> String {
    segments
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(encode_uri_component)
        .collect::<Vec<String>>()
        .join("/")
}

fn decode_segments(route: &str) -> Result<Vec<String>, NativePathError> {
    let mut decoded = Vec::new();
    for segment in route.split('/').filter(|segment| !segment.is_empty()) {
        let segment = decode_uri_component(segment)?;
        if segment.is_empty()
            || segment.contains('/')
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return Err(NativePathError::InvalidRouteSegment);
        }
        decoded.push(segment);
    }
    Ok(decoded)
}

fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED_PUNCTUATION.contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(UPPERCASE_HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(UPPERCASE_HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn decode_uri_component(value: &str) -> Result<String, NativePathError> {
    let bytes = value.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value);
            let low = bytes.get(index + 2).copied().and_then(hex_value);
            match (high, low) {
                (Some(high), Some(low)) => {
                    decoded.push((high << 4) | low);
                    index += 3;
                }
                _ => return Err(NativePathError::MalformedRouteEncoding),
            }
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| NativePathError::MalformedRouteEncoding)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_single_ascii_letter(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 1 && bytes[0].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::{decode_native_path_route, encode_native_path_route};
    use crate::host_platform::HostPlatform;
    use crate::native_path::normalize::NativePathError;

    #[test]
    fn a_posix_route_strips_the_leading_slash_and_escapes_the_rest() {
        assert_eq!(
            encode_native_path_route(HostPlatform::Linux, "/home/u/proj").as_deref(),
            Ok("home/u/proj")
        );
        assert_eq!(
            encode_native_path_route(HostPlatform::Linux, "/a b/c+d").as_deref(),
            Ok("a%20b/c%2Bd")
        );
        assert_eq!(
            encode_native_path_route(HostPlatform::Linux, "/").as_deref(),
            Ok("")
        );
    }

    #[test]
    fn a_posix_route_does_not_normalize_what_it_encodes() {
        assert_eq!(
            encode_native_path_route(HostPlatform::Linux, "/a/../b").as_deref(),
            Ok("a/../b")
        );
    }

    #[test]
    fn a_unreserved_character_survives_the_round_trip_unescaped() {
        assert_eq!(
            encode_native_path_route(HostPlatform::Linux, "/it's (fine)~.txt").as_deref(),
            Ok("it's%20(fine)~.txt")
        );
    }

    #[test]
    fn a_posix_route_decodes_to_an_absolute_path() {
        assert_eq!(
            decode_native_path_route(HostPlatform::Linux, "home/u/proj").as_deref(),
            Ok("/home/u/proj")
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Linux, "").as_deref(),
            Ok("/")
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Linux, "a%20b").as_deref(),
            Ok("/a b")
        );
    }

    #[test]
    fn a_malformed_escape_is_refused_rather_than_guessed() {
        assert_eq!(
            decode_native_path_route(HostPlatform::Linux, "a%2"),
            Err(NativePathError::MalformedRouteEncoding)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Linux, "a%zz"),
            Err(NativePathError::MalformedRouteEncoding)
        );
    }

    #[test]
    fn a_windows_drive_route_is_tagged_with_its_letter() {
        assert_eq!(
            encode_native_path_route(HostPlatform::Windows, r"C:\a b\c").as_deref(),
            Ok("~drive/C/a%20b/c")
        );
        assert_eq!(
            encode_native_path_route(HostPlatform::Windows, "C:/").as_deref(),
            Ok("~drive/C")
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~drive/C/a%20b/c").as_deref(),
            Ok("C:/a b/c")
        );
    }

    #[test]
    fn a_windows_unc_route_is_tagged_with_its_root() {
        assert_eq!(
            encode_native_path_route(HostPlatform::Windows, r"\\host\share\a").as_deref(),
            Ok("~unc/host/share/a")
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~unc/host/share/a").as_deref(),
            Ok("//host/share/a")
        );
    }

    #[test]
    fn a_windows_route_without_its_tag_is_refused() {
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "C/a"),
            Err(NativePathError::MissingTaggedRoot)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, ""),
            Err(NativePathError::MissingTaggedRoot)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~unc/host"),
            Err(NativePathError::InvalidUncRoute)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~drive"),
            Err(NativePathError::InvalidDriveRoute)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~drive/CC/a"),
            Err(NativePathError::InvalidDriveRoute)
        );
    }

    #[test]
    fn a_segment_that_would_re_split_itself_is_refused() {
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~drive/C/a%2Fb"),
            Err(NativePathError::InvalidRouteSegment)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~unc/host/share/a%5Cb"),
            Err(NativePathError::InvalidRouteSegment)
        );
        assert_eq!(
            decode_native_path_route(HostPlatform::Windows, "~unc/host/share/a%00b"),
            Err(NativePathError::InvalidRouteSegment)
        );
    }
}
