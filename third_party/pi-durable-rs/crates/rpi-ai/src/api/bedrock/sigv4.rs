//! Hand-written AWS Signature Version 4 request signing, reversed from the
//! pinned `@smithy/signature-v4` sources (`external/pi/node_modules/@smithy/
//! signature-v4/dist-es/`, pi 0.82.1 @ 2efa728) since the upstream adapter
//! delegates signing to `@aws-sdk/client-bedrock-runtime` (design §14:
//! hand-written SigV4, no aws-sdk crates).
//!
//! Ported behaviors (function-for-function from the smithy sources):
//! - `prepareRequest`: `authorization` / `x-amz-date` / `date` inputs are
//!   dropped before signing (`GENERATED_HEADERS`).
//! - `x-amz-content-sha256` is set and signed (`applyChecksum` defaults to
//!   true in `SignatureV4Base`).
//! - `getCanonicalHeaders`: lowercase names, `trim()` + inner `\s+` collapsed
//!   to one space, sorted; `ALWAYS_UNSIGNABLE_HEADERS` plus `proxy-*` /
//!   `sec-*` are skipped.
//! - `getCanonicalPath`: segment normalization (empty/`.` dropped, `..` pops)
//!   then `escapeUri` of the whole path with `%2F` restored — i.e. the
//!   already-encoded request path is encoded a second time (double encoding,
//!   `uriEscapePath = true`).
//! - `getCanonicalQuery`: keys/values `escapeUri`-encoded, sorted by encoded
//!   key, `x-amz-signature` excluded.
//! - String-to-sign and the `AWS4{secret}` HMAC-SHA256 key chain
//!   (`credentialDerivation.ts`), scope `{date}/{region}/{service}/aws4_request`.
//!
//! Intentional differences: only single-request signing is ported (no
//! presigned URLs, no event-stream message signing, no SigV4a, no signing-key
//! cache); the signing instant is injected for deterministic tests.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// `ALGORITHM_IDENTIFIER`.
pub const ALGORITHM_IDENTIFIER: &str = "AWS4-HMAC-SHA256";
/// `SHA256_HEADER`.
pub const CONTENT_SHA256_HEADER: &str = "x-amz-content-sha256";
/// `AMZ_DATE_HEADER`.
pub const AMZ_DATE_HEADER: &str = "x-amz-date";
/// `TOKEN_HEADER`.
pub const SECURITY_TOKEN_HEADER: &str = "x-amz-security-token";
/// `AUTH_HEADER`.
pub const AUTHORIZATION_HEADER: &str = "authorization";

/// `KEY_TYPE_IDENTIFIER`.
const KEY_TYPE_IDENTIFIER: &str = "aws4_request";

/// `ALWAYS_UNSIGNABLE_HEADERS` (constants.ts).
const ALWAYS_UNSIGNABLE_HEADERS: [&str; 15] = [
    "authorization",
    "cache-control",
    "connection",
    "expect",
    "from",
    "keep-alive",
    "max-forwards",
    "pragma",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "x-amzn-trace-id",
];

/// `GENERATED_HEADERS` (constants.ts): removed by `prepareRequest`.
const GENERATED_HEADERS: [&str; 3] = ["authorization", "x-amz-date", "date"];

/// `AwsCredentials` (static credentials only; upstream's credential-provider
/// chain is resolved by the caller).
#[derive(Debug, Clone)]
pub struct SigV4Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// `escapeUri` (`@smithy/core/protocols/util-uri-escape`): RFC 3986
/// percent-encoding — unreserved `A-Za-z0-9-_.~` survive verbatim, every other
/// UTF-8 byte becomes `%XX` (uppercase hex).
pub fn escape_uri(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `extendedEncodeURIComponent` (`@smithy/core/protocols`): identical to
/// [`escape_uri`] — JS `encodeURIComponent` keeps `!'()*` and the extension
/// re-encodes them, netting the same unreserved set.
pub fn extended_encode_uri_component(input: &str) -> String {
    escape_uri(input)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any size"));
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// `getCanonicalPath` (`SignatureV4Base`, `uriEscapePath = true`): normalize
/// `.` / `..` / empty segments, then `escapeUri` with `%2F` restored.
pub fn canonical_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            segments.pop();
        } else {
            segments.push(segment);
        }
    }
    let mut normalized = String::new();
    if path.starts_with('/') {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if !segments.is_empty() && path.ends_with('/') {
        normalized.push('/');
    }
    escape_uri(&normalized).replace("%2F", "/")
}

/// `getCanonicalHeaders` + `getCanonicalHeaderList`: returns the sorted
/// `(name, value)` pairs that participate in the signature.
fn canonical_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut canonical: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.to_lowercase();
            let unsignable = ALWAYS_UNSIGNABLE_HEADERS.contains(&name.as_str())
                || name.starts_with("proxy-")
                || name.starts_with("sec-");
            if unsignable {
                return None;
            }
            // JS `value.trim().replace(/\s+/g, " ")`.
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            Some((name, value))
        })
        .collect();
    canonical.sort_by(|a, b| a.0.cmp(&b.0));
    canonical
}

/// `getCanonicalQuery`: sorted `encoded=encoded` pairs (`x-amz-signature`
/// excluded; converse-stream requests carry no query).
fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<String> = query
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("x-amz-signature"))
        .map(|(key, value)| format!("{}={}", escape_uri(key), escape_uri(value)))
        .collect();
    pairs.sort();
    pairs.join("&")
}

/// `formatDate` (`utilDate.ts` `iso8601` minus `-`/`:`): epoch seconds to
/// `YYYYMMDDTHHMMSSZ` (UTC).
pub fn format_amz_date(epoch_secs: u64) -> String {
    // Howard Hinnant's civil-from-days algorithm.
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60
    )
}

/// The request to sign (grouped to keep [`sign_request`] arity low).
#[derive(Debug, Clone)]
pub struct SigV4Request<'a> {
    pub method: &'a str,
    /// Already-encoded request path (e.g. `/model/{enc(modelId)}/converse-stream`).
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    pub payload: &'a [u8],
}

/// The full header set of the signed request (the input headers minus the
/// generated ones, plus `x-amz-date` / `x-amz-content-sha256` /
/// `x-amz-security-token` / `authorization`). `host` must be present in
/// `request.headers` — it participates in the signature.
///
/// `signRequest` (`SignatureV4.signRequest`) with `applyChecksum = true`
/// (the `SignatureV4Base` default the bedrock client uses).
pub fn sign_request(
    request: &SigV4Request<'_>,
    credentials: &SigV4Credentials,
    region: &str,
    service: &str,
    signing_epoch_secs: u64,
) -> Vec<(String, String)> {
    // prepareRequest: drop generated headers.
    let mut signed: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|(name, _)| !GENERATED_HEADERS.contains(&name.to_lowercase().as_str()))
        .cloned()
        .collect();

    let long_date = format_amz_date(signing_epoch_secs);
    let short_date = &long_date[..8];
    let scope = format!("{short_date}/{region}/{service}/{KEY_TYPE_IDENTIFIER}");

    signed.push((AMZ_DATE_HEADER.to_owned(), long_date.clone()));
    if let Some(token) = &credentials.session_token {
        signed.push((SECURITY_TOKEN_HEADER.to_owned(), token.clone()));
    }
    // getPayloadHash + applyChecksum: hash header is set AND signed.
    let payload_hash = sha256_hex(request.payload);
    signed.push((CONTENT_SHA256_HEADER.to_owned(), payload_hash.clone()));

    let canonical = canonical_headers(&signed);
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n\n{}\n{payload_hash}",
        request.method,
        canonical_path(request.path),
        canonical_query(request.query),
        canonical
            .iter()
            .map(|(name, value)| format!("{name}:{value}"))
            .collect::<Vec<_>>()
            .join("\n"),
        canonical
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";"),
    );
    let string_to_sign = format!(
        "{ALGORITHM_IDENTIFIER}\n{long_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // getSigningKey: HMAC chain over date/region/service/aws4_request.
    let mut key = hmac_sha256(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        short_date,
    );
    key = hmac_sha256(&key, region);
    key = hmac_sha256(&key, service);
    key = hmac_sha256(&key, KEY_TYPE_IDENTIFIER);
    let signature = hex(&hmac_sha256(&key, &string_to_sign));

    let signed_header_list = canonical
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    signed.push((
        AUTHORIZATION_HEADER.to_owned(),
        format!(
            "{ALGORITHM_IDENTIFIER} Credential={}/{scope}, SignedHeaders={signed_header_list}, Signature={signature}",
            credentials.access_key_id
        ),
    ));
    signed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs_credentials() -> SigV4Credentials {
        SigV4Credentials {
            access_key_id: "AKIDEXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: None,
        }
    }

    /// AWS SigV4 documentation example (IAM ListUsers, 2015-08-30T12:36:00Z),
    /// extended with the `x-amz-content-sha256` header the smithy signer adds
    /// (`applyChecksum` default). The expected signature was computed with the
    /// algorithm documented at
    /// <https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>
    /// (whose unsigned-payload variant reproduces the documented `5d672d79...`).
    #[test]
    fn test_sign_request_aws_docs_vector() {
        let headers = vec![
            (
                "content-type".to_owned(),
                "application/x-www-form-urlencoded; charset=utf-8".to_owned(),
            ),
            ("host".to_owned(), "iam.amazonaws.com".to_owned()),
        ];
        let signed = sign_request(
            &SigV4Request {
                method: "GET",
                path: "/",
                query: &[
                    ("Action".to_owned(), "ListUsers".to_owned()),
                    ("Version".to_owned(), "2010-05-08".to_owned()),
                ],
                headers: &headers,
                payload: b"",
            },
            &docs_credentials(),
            "us-east-1",
            "iam",
            1_440_938_160,
        );
        let authorization = signed
            .iter()
            .find(|(name, _)| name == AUTHORIZATION_HEADER)
            .map(|(_, value)| value.as_str())
            .expect("authorization header");
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature=dd479fa8a80364edf2119ec24bebde66712ee9c9cb2b0d92eb3ab9ccdc0c3947"
        );
    }

    /// Determinism pin for the Bedrock-shaped request: percent-encoded model
    /// id in the path (double-encoded in the canonical request), JSON payload
    /// hash signed via `x-amz-content-sha256`.
    #[test]
    fn test_sign_request_bedrock_shape() {
        let headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            (
                "host".to_owned(),
                "bedrock-runtime.us-east-1.amazonaws.com".to_owned(),
            ),
        ];
        let signed = sign_request(
            &SigV4Request {
                method: "POST",
                path: "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse-stream",
                query: &[],
                headers: &headers,
                payload: br#"{"messages":[{"role":"user","content":[{"text":"hi"}]}]}"#,
            },
            &docs_credentials(),
            "us-east-1",
            "bedrock",
            1_440_938_160,
        );
        let authorization = signed
            .iter()
            .find(|(name, _)| name == AUTHORIZATION_HEADER)
            .map(|(_, value)| value.as_str())
            .expect("authorization header");
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature=25c5459f2dbc67c7f602871d9fbe312b3c516d965f383f517afe9fe7712b9745"
        );
        // A different payload changes the signature.
        let other = sign_request(
            &SigV4Request {
                method: "POST",
                path: "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse-stream",
                query: &[],
                headers: &headers,
                payload: b"{}",
            },
            &docs_credentials(),
            "us-east-1",
            "bedrock",
            1_440_938_160,
        );
        assert_ne!(
            signed.iter().find(|(name, _)| name == AUTHORIZATION_HEADER),
            other.iter().find(|(name, _)| name == AUTHORIZATION_HEADER)
        );
    }

    #[test]
    fn test_format_amz_date() {
        assert_eq!(format_amz_date(1_440_938_160), "20150830T123600Z");
        assert_eq!(format_amz_date(0), "19700101T000000Z");
        assert_eq!(format_amz_date(1_704_067_199), "20231231T235959Z");
    }

    #[test]
    fn test_escape_uri() {
        assert_eq!(escape_uri("abc-_.~"), "abc-_.~");
        assert_eq!(escape_uri("a:b/c d"), "a%3Ab%2Fc%20d");
        assert_eq!(extended_encode_uri_component("!'()*"), "%21%27%28%29%2A");
    }

    #[test]
    fn test_canonical_path_double_encodes() {
        assert_eq!(
            canonical_path("/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse-stream"),
            "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%253A0/converse-stream"
        );
        // Segment normalization happens before encoding.
        assert_eq!(canonical_path("/a/./b/../c"), "/a/c");
    }

    #[test]
    fn test_canonical_headers_filters_and_normalizes() {
        let headers = vec![
            ("X-Custom".to_owned(), "  a\t b  ".to_owned()),
            ("user-agent".to_owned(), "aws-sdk-js".to_owned()),
            ("proxy-auth".to_owned(), "x".to_owned()),
            ("sec-websocket".to_owned(), "y".to_owned()),
            ("Host".to_owned(), "h".to_owned()),
        ];
        let canonical = canonical_headers(&headers);
        assert_eq!(
            canonical,
            vec![
                ("host".to_owned(), "h".to_owned()),
                ("x-custom".to_owned(), "a b".to_owned()),
            ]
        );
    }
}
