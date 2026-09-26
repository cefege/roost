//! The Deepgram dictation settings, and the lifecycle of the one provider call
//! this domain makes. Reached as `core.services.telemetry.transcription`;
//! called by `diagnostics/rpc_transcription.rs`.
//!
//! WHAT A TRANSCRIPTION RESULT IS HERE, because the name misleads. The
//! coordinator does not transcribe: the browser opens Deepgram's listen socket
//! itself with the key this domain hands it, so the only third-party call in the
//! domain is the reachability probe behind `TranscriptionTest`. What is modelled
//! here is that probe's lifecycle rather than a background transcription job:
//! a probe is the only work here a third party can hold open.

use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::rpc::service::now_ms;

/// The `app_settings` key the stored Deepgram API key lives under.
pub const DEEPGRAM_KEY_SETTING: &str = "transcription.deepgram_key";

/// The `app_settings` key the dictation language lives under.
pub const DEEPGRAM_LANGUAGE_SETTING: &str = "transcription.deepgram_language";

/// The language a config that never set one reads as (`transcription.ts:17`).
pub const DEFAULT_LANGUAGE: &str = "en";

/// How long a provider that has not answered is given before the probe is
/// declared unanswered.
///
/// The bound is the point, not a tuning knob: a probe that can hang turns
/// Deepgram's latency into the caller's, and the caller is a Connect request on
/// a browser's socket.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(10);

/// A plain authenticated GET that any key able to transcribe passes.
const DEEPGRAM_PROJECTS_URL: &str = "https://api.deepgram.com/v1/projects";

/// How much of a key a masked config shows, so the operator can tell which key
/// is configured without the response carrying a usable credential.
const MASK_TAIL_CHARS: usize = 4;
const MASK_PREFIX: &str = "····";

/// The settings as a browser sees them: never the key itself, only whether one
/// is configured and which key it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionConfig {
    /// Whether a usable key is stored.
    pub deepgram_configured: bool,
    /// `····` plus the key's last four characters, empty when unset.
    pub deepgram_key_masked: String,
    /// The dictation language, never empty.
    pub deepgram_language: String,
}

/// Why the transcription settings could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptionStoreError {
    /// No key is configured, so there is nothing to hand a browser. A state, not
    /// a fault: a browser asking before a key is pasted must be told to stop.
    #[error("no Deepgram key is configured")]
    NotConfigured,
    /// The `app_settings` rows could not be read or written.
    #[error("transcription settings: {0}")]
    Store(sqlx::Error),
}

/// One reachability attempt, from before the request leaves to after it lands.
///
/// [`Self::Unreachable`] and [`Self::Unanswered`] are both terminal failures and
/// are deliberately different states: the first is the provider's own answer,
/// the second the absence of one, and only the second is worth retrying.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderProbe {
    /// No probe has run in this process.
    Idle,
    /// A probe is in flight against the provider.
    Pending {
        /// When the probe started.
        started_ms: i64,
    },
    /// The provider accepted the stored key at `finished_ms`.
    Reachable { finished_ms: i64 },
    /// The provider answered, and refused, at `finished_ms`.
    Unreachable { reason: String, finished_ms: i64 },
    /// The provider did not answer inside [`PROBE_DEADLINE`]: `after_ms` is the
    /// bound that elapsed, `finished_ms` when it was observed.
    Unanswered { after_ms: i64, finished_ms: i64 },
}

impl ProviderProbe {
    /// The terminal failure's reason, for a caller that renders one line. `None`
    /// for every state that is not a failure, [`Self::Idle`] and [`Self::Pending`]
    /// included: a probe that has not answered is not a failure.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        match self {
            Self::Unreachable { reason, .. } => Some(reason.clone()),
            Self::Unanswered { after_ms, .. } => {
                Some(format!("Deepgram did not answer within {after_ms}ms"))
            }
            Self::Idle | Self::Pending { .. } | Self::Reachable { .. } => None,
        }
    }

    /// Whether this state is terminal: no further transition will happen.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Idle | Self::Pending { .. })
    }
}

/// Why one provider call did not reach [`ProviderProbe::Reachable`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderRefusal {
    /// The provider answered, and the answer was not acceptance.
    Refused(String),
    /// The call did not complete inside [`PROBE_DEADLINE`]. Kept apart from
    /// [`Self::Refused`] because the two demand opposite responses: a refusal
    /// means the key is wrong, an unanswered provider means the key says
    /// nothing at all.
    Unanswered { after_ms: i64 },
}

impl ProviderRefusal {
    /// The terminal state this refusal settles on.
    fn into_probe(self, finished_ms: i64) -> ProviderProbe {
        match self {
            Self::Refused(reason) => ProviderProbe::Unreachable {
                reason,
                finished_ms,
            },
            Self::Unanswered { after_ms } => ProviderProbe::Unanswered {
                after_ms,
                finished_ms,
            },
        }
    }
}

/// The provider call's in-flight future, boxed so a probe sender is a plain
/// `Fn` rather than a second trait with a lifetime in it.
pub type ProbeFuture = Pin<Box<dyn Future<Output = Result<(), ProviderRefusal>> + Send>>;

/// How one reachability attempt is made: given the stored key, answer.
pub type ProbeSender = Arc<dyn Fn(String) -> ProbeFuture + Send + Sync>;

/// The transcription domain's process state. `new()` stays zero-argument.
pub struct TranscriptionRuntime {
    /// The probe lifecycle. A `std::sync::Mutex` because both critical sections
    /// are a field read or write -- neither spans an await.
    probe: Mutex<ProviderProbe>,
    /// How a probe reaches the provider. Installable so a caller can drive the
    /// lifecycle without a third party at the other end of it.
    sender: Mutex<ProbeSender>,
}

impl TranscriptionRuntime {
    /// A runtime that probes Deepgram over HTTPS.
    #[must_use]
    pub fn new() -> Self {
        let sender = match reqwest::Client::builder().build() {
            Ok(client) => Arc::new(deepgram_projects_probe(client)) as ProbeSender,
            Err(error) => Arc::new(unusable_probe(format!(
                "the Deepgram HTTP client could not be built: {error}"
            ))),
        };
        Self {
            probe: Mutex::new(ProviderProbe::Idle),
            sender: Mutex::new(sender),
        }
    }

    /// Answer future probes through `sender` instead of over HTTPS.
    pub fn set_probe_sender(&self, sender: ProbeSender) {
        *lock(&self.sender) = sender;
    }

    /// The probe's current state, as an observer sees it.
    #[must_use]
    pub fn provider_probe(&self) -> ProviderProbe {
        lock(&self.probe).clone()
    }

    /// Run one reachability attempt against `key` and settle on a terminal
    /// state. The caller awaits this, so the outcome is what the RPC returns;
    /// the state is kept too, because a probe left pending forever reads to
    /// anything inspecting the process afterwards exactly like a working one.
    pub async fn probe_provider(&self, key: String) -> ProviderProbe {
        let started_ms = now_ms();
        *lock(&self.probe) = ProviderProbe::Pending { started_ms };
        info!(target: "transcription", started_ms, "provider probe started");

        let sender = lock(&self.sender).clone();
        let outcome = match tokio::time::timeout(PROBE_DEADLINE, sender(key)).await {
            Ok(Ok(())) => ProviderProbe::Reachable {
                finished_ms: now_ms(),
            },
            Ok(Err(refusal)) => refusal.into_probe(now_ms()),
            Err(_elapsed) => ProviderRefusal::Unanswered {
                after_ms: PROBE_DEADLINE.as_millis() as i64,
            }
            .into_probe(now_ms()),
        };

        *lock(&self.probe) = outcome.clone();
        match outcome.failure_reason() {
            Some(reason) => {
                info!(target: "transcription", ?outcome, reason, "provider probe failed");
            }
            None => info!(target: "transcription", ?outcome, "provider probe settled"),
        }
        outcome
    }
}

impl Default for TranscriptionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TranscriptionRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranscriptionRuntime")
            .field("probe", &self.provider_probe())
            .finish_non_exhaustive()
    }
}

/// The settings as stored for one tenant.
pub async fn load_config(
    pool: &SqlitePool,
    dashboard_id: &str,
) -> Result<TranscriptionConfig, TranscriptionStoreError> {
    let key = stored_key(pool, dashboard_id)
        .await?
        .map_or_else(String::new, |key| mask(&key));
    let language = match read_setting(pool, dashboard_id, DEEPGRAM_LANGUAGE_SETTING).await? {
        Some(language) if !language.is_empty() => language,
        _ => DEFAULT_LANGUAGE.to_owned(),
    };
    Ok(TranscriptionConfig {
        deepgram_configured: !key.is_empty(),
        deepgram_key_masked: key,
        deepgram_language: language,
    })
}

/// Write the settings and return them as they now stand.
///
/// `key` is the proto3 optional: absent leaves the stored key alone, present
/// overwrites it, and present-and-empty clears it.
pub async fn store_config(
    pool: &SqlitePool,
    dashboard_id: &str,
    key: Option<&str>,
    language: &str,
) -> Result<TranscriptionConfig, TranscriptionStoreError> {
    if let Some(key) = key {
        put_setting(pool, dashboard_id, DEEPGRAM_KEY_SETTING, key.trim()).await?;
    }
    let language = language.trim();
    put_setting(
        pool,
        dashboard_id,
        DEEPGRAM_LANGUAGE_SETTING,
        if language.is_empty() {
            DEFAULT_LANGUAGE
        } else {
            language
        },
    )
    .await?;
    load_config(pool, dashboard_id).await
}

/// The stored Deepgram key, or `None` when none is configured.
pub async fn stored_key(
    pool: &SqlitePool,
    dashboard_id: &str,
) -> Result<Option<String>, TranscriptionStoreError> {
    match read_setting(pool, dashboard_id, DEEPGRAM_KEY_SETTING).await? {
        Some(key) if !key.is_empty() => Ok(Some(key)),
        _ => Ok(None),
    }
}

/// One `app_settings` row, by exact key and tenant.
///
/// Scoped to the tenant the writes are stamped with, not to the
/// `transcription.%` prefix v2 matched: a prefix read would let another
/// dashboard's row answer for this one.
async fn read_setting(
    pool: &SqlitePool,
    dashboard_id: &str,
    key: &str,
) -> Result<Option<String>, TranscriptionStoreError> {
    let row = sqlx::query("SELECT value FROM app_settings WHERE dashboard_id = ?1 AND key = ?2")
        .bind(dashboard_id)
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(TranscriptionStoreError::Store)?;
    row.map(|row| row.try_get("value").map_err(TranscriptionStoreError::Store))
        .transpose()
}

/// Upsert one tenant-scoped `app_settings` row.
async fn put_setting(
    pool: &SqlitePool,
    dashboard_id: &str,
    key: &str,
    value: &str,
) -> Result<(), TranscriptionStoreError> {
    sqlx::query(
        "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (dashboard_id, key) DO UPDATE SET value = ?3, updated_at_ms = ?4",
    )
    .bind(dashboard_id)
    .bind(key)
    .bind(value)
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(TranscriptionStoreError::Store)?;
    Ok(())
}

/// `····` plus the key's last four characters.
fn mask(key: &str) -> String {
    let tail: String = key
        .chars()
        .skip(key.chars().count().saturating_sub(MASK_TAIL_CHARS))
        .collect();
    format!("{MASK_PREFIX}{tail}")
}

/// The reachability call: a plain authenticated GET, so it passes for any key
/// that can transcribe, which is the only question the Test button asks.
fn deepgram_projects_probe(client: reqwest::Client) -> impl Fn(String) -> ProbeFuture {
    // The sender is `Fn` and the future is `'static`, so it can neither move its
    // capture into the future nor borrow one. An `Arc` satisfies both, and a
    // clone is a pointer bump: `reqwest::Client` already reference-counts.
    let client = Arc::new(client);
    move |key: String| {
        let client = Arc::clone(&client);
        Box::pin(async move {
            let response = client
                .get(DEEPGRAM_PROJECTS_URL)
                .header("Authorization", format!("Token {key}"))
                .send()
                .await
                .map_err(|error| ProviderRefusal::Refused(transport_reason(&error)))?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            let code = status.as_u16();
            Err(ProviderRefusal::Refused(if code == 401 || code == 403 {
                format!("Key rejected by Deepgram ({code})")
            } else {
                format!("Deepgram returned {code}")
            }))
        })
    }
}

/// Why a request never reached Deepgram, in the operator's words.
fn transport_reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "Deepgram did not answer in time".to_owned()
    } else if error.is_connect() {
        "could not reach Deepgram".to_owned()
    } else {
        error.to_string()
    }
}

/// A probe sender for a client that could not be built, so the operator is told
/// why rather than handed a probe that never runs.
fn unusable_probe(reason: String) -> impl Fn(String) -> ProbeFuture {
    move |_key: String| {
        let reason = reason.clone();
        Box::pin(async move { Err(ProviderRefusal::Refused(reason)) })
    }
}

/// A lock that recovers from a poisoned mutex. Nothing guarded here is left
/// half-written by a panic, and the alternative is a dead coordinator.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
