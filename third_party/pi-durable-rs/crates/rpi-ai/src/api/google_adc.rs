//! Reverse-engineered port of the Application Default Credentials (ADC)
//! subset of `google-auth-library` v10.6.2 (pinned under
//! `external/pi/node_modules/google-auth-library`), as exercised by
//! `@google/genai` 1.52.0 `NodeAuth` in Vertex AI mode
//! (`NodeAuth.addGoogleAuthHeaders` → `GoogleAuth.getRequestHeaders` →
//! `authorization: Bearer <access_token>`).
//!
//! Upstream has no pi-side source file for this logic; pi only passes
//! `googleAuthOptions: {keyFilename}` (from `GOOGLE_APPLICATION_CREDENTIALS`)
//! into the SDK (`google-vertex.ts` `buildGoogleAuthOptions`). This module
//! implements the credential resolution chain and token acquisition the SDK
//! delegates to google-auth-library:
//!
//! 1. `GOOGLE_APPLICATION_CREDENTIALS` file (same env var pi forwards as
//!    `keyFilename`; `GoogleAuth.#determineClient`/`_tryGetApplicationCredentialsFromEnvironmentVariable`),
//! 2. the well-known gcloud file
//!    (`_tryGetApplicationCredentialsFromWellKnownFile`),
//! 3. the GCE metadata server (`Compute` client; our single token request
//!    with a probe timeout replaces upstream's separate
//!    `gcp-metadata.isAvailable()` probe),
//! 4. otherwise the `NO_ADC_FOUND` error text.
//!
//! Supported credential JSON types:
//! - `service_account` → OAuth2 JWT-bearer grant (RFC 7523, RS256 assertion)
//!   to the token endpoint (`gtoken/getToken.js`: fixed
//!   `https://oauth2.googleapis.com/token`, payload
//!   `{iss, scope, aud, exp: iat + 3600, iat}`), scope fixed to
//!   `cloud-platform` (`NodeAuth` `REQUIRED_VERTEX_AI_SCOPE`),
//! - `authorized_user` → `refresh_token` grant (`oauth2client.js`
//!   `refreshTokenNoCache`).
//!
//! Intentional differences (see D-024):
//! - `external_account` / `external_account_authorized_user` /
//!   `impersonated_service_account` credential files are rejected with an
//!   explicit error instead of being resolved (workload-identity federation
//!   chains are out of scope),
//! - the JSON-parse failure PEM/p12 fallback in `fromStreamAsync` (a GAPIC
//!   legacy path pi never exercises) is not ported; a non-JSON key file is an
//!   error,
//! - token-endpoint failures use the gtoken `{error}: {error_description}`
//!   wording when the body carries an `error` field, otherwise an HTTP
//!   status summary (approximation of the gaxios error surface),
//! - `AdcEndpoints` is a `#[doc(hidden)]` test seam; upstream pins the same
//!   values as constants,
//! - tokens are resolved per `stream()` call, mirroring pi constructing a
//!   fresh `GoogleGenAI`/`GoogleAuth` per call (no cross-call cache).

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::transport_error::format_reqwest_error;

/// `REQUIRED_VERTEX_AI_SCOPE` (`NodeAuth`): the only scope pi ever requests.
pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// `GOOGLE_TOKEN_URL` (`gtoken/getToken.js`, fixed since google-auth-library
/// v10; the credential file's `token_uri` is NOT consulted).
pub const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// `Compute` token path on the metadata server (`gcp-metadata`
/// `HOST_ADDRESS` + `computeclient.js` `service-accounts/default/token`).
pub const DEFAULT_METADATA_TOKEN_URL: &str =
    "http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/token";

/// `GoogleAuthExceptionMessages.NO_ADC_FOUND`, verbatim.
pub const NO_ADC_FOUND_MESSAGE: &str = "Could not load the default credentials. Browse to https://cloud.google.com/docs/authentication/getting-started for more information.";

/// `GOOGLE_GRANT_TYPE` (`gtoken/getToken.js`).
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// Upstream's GCE availability probe timeout (`gcp-metadata`
/// `isAvailable`, 3s); applied to the metadata token request, which doubles
/// as the probe in this port.
const METADATA_TIMEOUT: Duration = Duration::from_secs(3);

/// ADC endpoints. `Default` carries the production constants;
/// `#[doc(hidden)]` because overriding them exists only as a test seam
/// (upstream pins them as module constants).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct AdcEndpoints {
    /// OAuth2 token endpoint for both grant types.
    pub token_url: String,
    /// GCE metadata server token URL.
    pub metadata_token_url: String,
    /// Well-known gcloud credential file override; `None` performs the
    /// platform default lookup (`~/.config/gcloud/...`).
    pub well_known_file: Option<PathBuf>,
}

impl Default for AdcEndpoints {
    fn default() -> Self {
        Self {
            token_url: DEFAULT_TOKEN_URL.to_owned(),
            metadata_token_url: DEFAULT_METADATA_TOKEN_URL.to_owned(),
            well_known_file: None,
        }
    }
}

/// A parsed ADC credential file (`GoogleAuth.fromJSON` dispatch on `type`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdcCredentials {
    /// `type == "service_account"` → JWT client.
    ServiceAccount {
        client_email: String,
        /// PEM-encoded PKCS#8 RSA private key.
        private_key: String,
    },
    /// `type == "authorized_user"` → UserRefreshClient.
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
}

fn require_str<'a>(json: &'a Value, field: &str, kind: &str) -> Result<&'a str, String> {
    json.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("The {kind} credential file is missing the `{field}` field."))
}

/// `GoogleAuth.fromJSON`: dispatch on the credential `type`. Unknown types
/// fall into the JWT branch upstream; we only accept the two shapes pi can
/// meaningfully use and reject the federation types explicitly.
pub fn parse_credential_file(contents: &str) -> Result<AdcCredentials, String> {
    let json: Value = serde_json::from_str(contents)
        .map_err(|error| format!("The credential file is not valid JSON: {error}"))?;
    let credential_type = json.get("type").and_then(Value::as_str).unwrap_or("");
    match credential_type {
        "service_account" => Ok(AdcCredentials::ServiceAccount {
            client_email: require_str(&json, "client_email", "service_account")?.to_owned(),
            private_key: require_str(&json, "private_key", "service_account")?.to_owned(),
        }),
        "authorized_user" => Ok(AdcCredentials::AuthorizedUser {
            client_id: require_str(&json, "client_id", "authorized_user")?.to_owned(),
            client_secret: require_str(&json, "client_secret", "authorized_user")?.to_owned(),
            refresh_token: require_str(&json, "refresh_token", "authorized_user")?.to_owned(),
        }),
        other => Err(format!(
            "Unsupported credential type `{other}` for google-vertex ADC: only `service_account` and `authorized_user` credential files are supported."
        )),
    }
}

/// The platform default well-known file
/// (`_tryGetApplicationCredentialsFromWellKnownFile`): `$HOME/.config/gcloud/
/// application_default_credentials.json` on Linux/macOS, `%APPDATA%\gcloud\
/// application_default_credentials.json` on Windows.
fn default_well_known_file() -> Option<PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let root = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"));
    root.map(|dir| {
        dir.join("gcloud")
            .join("application_default_credentials.json")
    })
}

/// Base64url without padding (JWT/JWS encoding).
fn base64url(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Extracts the DER body of a `-----BEGIN PRIVATE KEY-----` PEM block
/// (Google service-account keys are PKCS#8).
fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let begin = pem
        .find("-----BEGIN PRIVATE KEY-----")
        .ok_or_else(|| "The service account private_key is not a PKCS#8 PEM block.".to_owned())?;
    let rest = &pem[begin + "-----BEGIN PRIVATE KEY-----".len()..];
    let end = rest
        .find("-----END PRIVATE KEY-----")
        .ok_or_else(|| "The service account private_key PEM block is not terminated.".to_owned())?;
    let base64_body: String = rest[..end].chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(base64_body)
        .map_err(|error| {
            format!("The service account private_key PEM body is not valid base64: {error}")
        })
}

/// `gtoken/jwsSign.js` `buildPayloadForJwsSign` + `getJwsSign`: an RS256 JWS
/// over `{iss, scope, aud, exp: iat + 3600, iat}` (no `sub` — upstream's
/// `sub: undefined` is dropped by `JSON.stringify`; object key order matches
/// the JS literal).
fn build_jwt_assertion(
    client_email: &str,
    private_key_pem: &str,
    token_url: &str,
    now_secs: u64,
) -> Result<String, String> {
    let header = json!({"alg": "RS256", "typ": "JWT"});
    let mut payload = Map::new();
    payload.insert("iss".to_owned(), json!(client_email));
    payload.insert("scope".to_owned(), json!(CLOUD_PLATFORM_SCOPE));
    payload.insert("aud".to_owned(), json!(token_url));
    payload.insert("exp".to_owned(), json!(now_secs + 3600));
    payload.insert("iat".to_owned(), json!(now_secs));

    let signing_input = format!(
        "{}.{}",
        base64url(
            serde_json::to_string(&header)
                .unwrap_or_default()
                .as_bytes()
        ),
        base64url(
            serde_json::to_string(&Value::Object(payload))
                .unwrap_or_default()
                .as_bytes()
        ),
    );

    let der = pem_to_der(private_key_pem)?;
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der)
        .map_err(|_| "The service account private_key is not a valid PKCS#8 RSA key.".to_owned())?;
    let rng = ring::rand::SystemRandom::new();
    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &rng,
            signing_input.as_bytes(),
            &mut signature,
        )
        .map_err(|_| "Failed to sign the service account JWT assertion.".to_owned())?;
    Ok(format!("{signing_input}.{}", base64url(&signature)))
}

/// Extracts `access_token` from a token endpoint / metadata response body.
fn parse_token_response(body: &str, source: &str) -> Result<String, String> {
    let json: Value = serde_json::from_str(body)
        .map_err(|error| format!("The {source} token response is not valid JSON: {error}"))?;
    json.get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("The {source} token response does not contain an access_token."))
}

/// Formats a failed token-endpoint response: gtoken's
/// `{error}: {error_description}` wording when the body carries an `error`
/// field, otherwise an HTTP status summary.
fn token_error_message(status: u16, body: &str) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(body)
        && let Some(error) = json.get("error").and_then(Value::as_str)
    {
        let description = json
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return format!("{error}: {description}");
    }
    format!("Token request failed with status {status}.")
}

/// POSTs a form-encoded grant to the token endpoint and extracts the access
/// token. No retries (the SDK's gaxios retry config for the token request is
/// not ported).
async fn post_token_grant(
    client: &reqwest::Client,
    token_url: &str,
    form: &[(&str, &str)],
) -> Result<String, String> {
    let response = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|error| format_reqwest_error(&error))?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(token_error_message(status, &body));
    }
    parse_token_response(&body, "OAuth2")
}

/// Exchanges the credentials for an access token.
async fn fetch_access_token(
    credentials: &AdcCredentials,
    endpoints: &AdcEndpoints,
) -> Result<String, String> {
    let client = crate::api::http_client::adapter_client_builder(None, &endpoints.token_url)?
        .build()
        .map_err(|error| error.to_string())?;
    match credentials {
        AdcCredentials::ServiceAccount {
            client_email,
            private_key,
        } => {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let assertion =
                build_jwt_assertion(client_email, private_key, &endpoints.token_url, now_secs)?;
            post_token_grant(
                &client,
                &endpoints.token_url,
                &[
                    ("grant_type", JWT_BEARER_GRANT_TYPE),
                    ("assertion", assertion.as_str()),
                ],
            )
            .await
        }
        AdcCredentials::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
        } => {
            post_token_grant(
                &client,
                &endpoints.token_url,
                &[
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("refresh_token", refresh_token.as_str()),
                    ("grant_type", "refresh_token"),
                ],
            )
            .await
        }
    }
}

/// `Compute.refreshTokenNoCache`: GET the metadata server token endpoint with
/// the `Metadata-Flavor: Google` header. The request doubles as the GCE
/// availability probe (3s timeout, like `gcp-metadata.isAvailable`).
async fn fetch_metadata_access_token(endpoints: &AdcEndpoints) -> Result<String, String> {
    let client =
        crate::api::http_client::adapter_client_builder(None, &endpoints.metadata_token_url)?
            .timeout(METADATA_TIMEOUT)
            .build()
            .map_err(|error| error.to_string())?;
    let response = client
        .get(&endpoints.metadata_token_url)
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .map_err(|error| {
            format!(
                "Could not refresh access token: {}",
                format_reqwest_error(&error)
            )
        })?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!(
            "Could not refresh access token: metadata server responded with status {status}."
        ));
    }
    parse_token_response(&body, "metadata server")
}

/// Resolves an ADC access token following google-auth-library's chain:
/// `GOOGLE_APPLICATION_CREDENTIALS` → well-known gcloud file → GCE metadata
/// server → `NO_ADC_FOUND`. Read errors on the env-var path are wrapped with
/// the upstream `Unable to read the credential file specified by the
/// GOOGLE_APPLICATION_CREDENTIALS environment variable: …` prefix.
pub async fn resolve_access_token(
    env: Option<&ProviderEnv>,
    endpoints: &AdcEndpoints,
) -> Result<String, String> {
    if let Some(path) = get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env) {
        let contents = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "Unable to read the credential file specified by the GOOGLE_APPLICATION_CREDENTIALS environment variable: {error}"
            )
        })?;
        let credentials = parse_credential_file(&contents).map_err(|error| {
            format!(
                "Unable to read the credential file specified by the GOOGLE_APPLICATION_CREDENTIALS environment variable: {error}"
            )
        })?;
        return fetch_access_token(&credentials, endpoints).await;
    }

    let well_known = endpoints
        .well_known_file
        .clone()
        .or_else(default_well_known_file);
    if let Some(path) = well_known
        && path.is_file()
    {
        let contents = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "Unable to read the credential file at {}: {error}",
                path.display()
            )
        })?;
        let credentials = parse_credential_file(&contents)?;
        return fetch_access_token(&credentials, endpoints).await;
    }

    if let Ok(token) = fetch_metadata_access_token(endpoints).await {
        return Ok(token);
    }
    Err(NO_ADC_FOUND_MESSAGE.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Throwaway 2048-bit RSA key generated for these tests only (openssl
    /// genpkey + pkcs8 -topk8 -nocrypt); never used anywhere else.
    const TEST_PRIVATE_KEY: &str = include_str!("../../tests/fixtures/adc_test_key.pem");

    #[test]
    fn test_pem_to_der_roundtrip() {
        let der = pem_to_der(TEST_PRIVATE_KEY).expect("der");
        assert!(der.len() > 1000, "PKCS#8 DER body should be sizeable");
    }

    #[test]
    fn test_pem_to_der_rejects_non_pem() {
        assert!(pem_to_der("not a pem").is_err());
    }

    #[test]
    fn test_build_jwt_assertion_structure() {
        let assertion = build_jwt_assertion(
            "svc@example.iam.gserviceaccount.com",
            TEST_PRIVATE_KEY,
            DEFAULT_TOKEN_URL,
            1_700_000_000,
        )
        .expect("assertion");
        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3, "JWS compact form has three segments");

        use base64::Engine;
        let decode = |segment: &str| {
            String::from_utf8(
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(segment)
                    .expect("base64url"),
            )
            .expect("utf8")
        };
        let header: Value = serde_json::from_str(&decode(parts[0])).expect("header json");
        assert_eq!(header, json!({"alg": "RS256", "typ": "JWT"}));
        let payload: Value = serde_json::from_str(&decode(parts[1])).expect("payload json");
        assert_eq!(
            payload,
            json!({
                "iss": "svc@example.iam.gserviceaccount.com",
                "scope": CLOUD_PLATFORM_SCOPE,
                "aud": DEFAULT_TOKEN_URL,
                "exp": 1_700_000_000u64 + 3600,
                "iat": 1_700_000_000u64,
            })
        );
        assert!(!parts[2].is_empty(), "signature segment present");
    }

    #[test]
    fn test_build_jwt_assertion_verifies_with_public_key() {
        let assertion = build_jwt_assertion(
            "svc@example.iam.gserviceaccount.com",
            TEST_PRIVATE_KEY,
            DEFAULT_TOKEN_URL,
            1_700_000_000,
        )
        .expect("assertion");
        let parts: Vec<&str> = assertion.split('.').collect();
        let der = pem_to_der(TEST_PRIVATE_KEY).expect("der");
        let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der).expect("key pair");
        use base64::Engine;
        use ring::signature::KeyPair;
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .expect("signature base64url");
        let public_key = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            key_pair.public_key().as_ref(),
        );
        public_key
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .expect("RS256 signature verifies against the public key");
    }

    #[test]
    fn test_parse_credential_file_service_account() {
        let credentials = parse_credential_file(
            r#"{"type": "service_account", "client_email": "a@b", "private_key": "pem"}"#,
        )
        .expect("parsed");
        assert_eq!(
            credentials,
            AdcCredentials::ServiceAccount {
                client_email: "a@b".to_owned(),
                private_key: "pem".to_owned(),
            }
        );
    }

    #[test]
    fn test_parse_credential_file_authorized_user() {
        let credentials = parse_credential_file(
            r#"{"type": "authorized_user", "client_id": "id", "client_secret": "secret", "refresh_token": "rt"}"#,
        )
        .expect("parsed");
        assert_eq!(
            credentials,
            AdcCredentials::AuthorizedUser {
                client_id: "id".to_owned(),
                client_secret: "secret".to_owned(),
                refresh_token: "rt".to_owned(),
            }
        );
    }

    #[test]
    fn test_parse_credential_file_rejects_external_account() {
        let error =
            parse_credential_file(r#"{"type": "external_account", "audience": "x"}"#).unwrap_err();
        assert!(
            error.contains("external_account"),
            "error names the type: {error}"
        );
    }

    #[test]
    fn test_parse_credential_file_rejects_missing_fields() {
        assert!(parse_credential_file(r#"{"type": "service_account"}"#).is_err());
        assert!(parse_credential_file("not json").is_err());
    }

    #[test]
    fn test_token_error_message_prefers_error_fields() {
        assert_eq!(
            token_error_message(
                400,
                r#"{"error": "invalid_grant", "error_description": "Bad"}"#
            ),
            "invalid_grant: Bad"
        );
        assert_eq!(
            token_error_message(500, "oops"),
            "Token request failed with status 500."
        );
    }

    #[test]
    fn test_parse_token_response() {
        assert_eq!(
            parse_token_response(r#"{"access_token": "tok", "expires_in": 3600}"#, "test")
                .expect("token"),
            "tok"
        );
        assert!(parse_token_response(r#"{"expires_in": 3600}"#, "test").is_err());
        assert!(parse_token_response("nope", "test").is_err());
    }
}
