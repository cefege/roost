//! Port of `packages/ai/src/models.ts` @ pi 0.82.1 (2efa728) — T03 scope:
//! `Provider` / `ProviderStreams` traits, `create_provider` with api-map
//! dispatch, `Models` (auth application + stream/stream_simple dispatch),
//! thinking-level helpers. `login` / `logout` landed here with the T13 login
//! wiring (`Models::login` / `Models::logout`).
//!
//! Out of T03 scope (upstream features landing later): `checkAuth` /
//! `getAvailable` (T04). `filterModels` landed in T13 W4
//! as a [`Provider`] trait method; the availability surface that applies it
//! (`get_available`, model-runtime.ts:240-268 → models.ts:394-408) lives in
//! rpi's model runtime (`crates/rpi/src/core/model_runtime.rs`), not on
//! `Models`. The dynamic provider overlay (`refreshModels` /
//! `inflightRefresh` dedup, models.ts:276-328) landed in T13 W6-C:
//! [`Models::refresh`], [`Provider::refresh_models`], [`InflightRefresh`]
//! and the [`crate::models_store`] plumbing. `CreateProviderOptions.
//! fetchModels` is deferred to the provider-composer extension refresh
//! wiring (T15; see D-036) — the refresh pattern lives in the radius and
//! remote-catalog providers instead.
//!
//! Intentional differences:
//! - `ApiStreamOptions<TApi>` per-API extras collapse to `StreamOptions`
//!   (Azure extras arrive with the Azure adapter, T13).
//! - The trait for API implementations is named `ProviderStreams` (upstream
//!   name); design §3.3 calls it `ApiStream` — same concept.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::auth::{
    AuthContext, AuthInteraction, AuthOperationOptions, AuthResolutionOverrides, AuthResult,
    AuthType, Credential, CredentialStore, DefaultAuthContext, InMemoryCredentialStore,
    LoginOptions, ModelsError, ModelsErrorCode, ProviderAuth, resolve_provider_auth,
};
use crate::models_json::OrderedMap;
use crate::models_store::{
    InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ProviderModelsStore,
};
use crate::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierResult, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    ImageModel, ImagesContext, ImagesOptions, Model, ModelThinkingLevel, ProviderClassifier,
    ProviderEnv, ProviderHeaders, ProviderImageGenerator, SimpleStreamOptions, StreamOptions,
    TranscriptContext, classifier_error_result, image_error_result,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::merge_headers;

use super::api::lazy::lazy_stream;
use super::error::AiError;

/// `ProviderStreams` — the API-adapter interface (design §3.3 `ApiStream`).
/// Implementations return the event stream synchronously; once a stream is
/// returned, request/model/runtime failures are encoded as
/// `StreamEvent::Error` on the stream (never `Err`).
///
/// `stream_simple` may fail synchronously when request auth is missing
/// (types.ts:325-330 @ 9841914, 8b5899dce): "Direct `streamSimple()` calls
/// may throw synchronously when request auth is missing. Once a stream is
/// returned, request/model/runtime failures should be encoded in that
/// stream." The Rust equivalent of the upstream `throw` is the `Err`
/// return; aggregation layers (`Models::stream_simple` via `lazy_stream`)
/// convert it to an in-stream setup error event, matching upstream
/// `lazyStream`'s `.catch`.
pub trait ProviderStreams: Send + Sync {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String>;

    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        None
    }

    fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<'static, Result<(), String>>> {
        None
    }
}

/// `ModelsRequestTransforms` — the `transformHeaders` callback
/// (models.ts:78-81 @ 4181f66). Applied once to the fully assembled
/// model/auth/request headers before provider dispatch, on **all**
/// authenticated model requests (stream/complete/simple/deferred).
pub type ModelsRequestTransforms = Arc<
    dyn Fn(ProviderHeaders) -> futures::future::BoxFuture<'static, ProviderHeaders> + Send + Sync,
>;

/// `ModelsApiStreamOptions` = `StreamOptions` + `ModelsRequestTransforms`
/// (models.ts:83 @ 4181f66).
#[derive(Clone, Default)]
pub struct ModelsStreamOptions {
    pub stream: StreamOptions,
    pub transform_headers: Option<ModelsRequestTransforms>,
}

impl From<StreamOptions> for ModelsStreamOptions {
    fn from(stream: StreamOptions) -> Self {
        Self {
            stream,
            transform_headers: None,
        }
    }
}

/// `ModelsSimpleStreamOptions` = `SimpleStreamOptions` +
/// `ModelsRequestTransforms` (models.ts:84 @ 4181f66).
#[derive(Clone, Default)]
pub struct ModelsSimpleStreamOptions {
    pub simple: SimpleStreamOptions,
    pub transform_headers: Option<ModelsRequestTransforms>,
}

impl From<SimpleStreamOptions> for ModelsSimpleStreamOptions {
    fn from(simple: SimpleStreamOptions) -> Self {
        Self {
            simple,
            transform_headers: None,
        }
    }
}
/// Deferred fetch options plus the Models-only request-header transform.
#[derive(Clone, Default)]
pub struct ModelsDeferredFetchOptions {
    pub deferred: DeferredFetchOptions,
    pub transform_headers: Option<ModelsRequestTransforms>,
}

/// Deferred cancellation options plus the Models-only request-header transform.
#[derive(Clone, Default)]
pub struct ModelsDeferredCancelOptions {
    pub deferred: DeferredCancelOptions,
    pub transform_headers: Option<ModelsRequestTransforms>,
}

/// `Provider` — the concrete runtime unit (T03 subset, see module docs).
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> Option<&str>;
    fn headers(&self) -> Option<&ProviderHeaders>;
    fn auth(&self) -> &ProviderAuth;

    /// Current known models, sync. Must not panic; `Models` treats a panicking
    /// implementation as having no models.
    fn get_models(&self) -> Vec<Model>;

    /// `getAllModels?` (models.ts:174 @ abe508e1b / schema v6): every model
    /// type known to the provider (chat + image + classifier). Defaults to
    /// the chat-only [`Provider::get_models`] wrapped as [`AnyModel::Chat`],
    /// so existing providers need no change and chat reads keep their
    /// current behavior (red line).
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.get_models().into_iter().map(AnyModel::Chat).collect()
    }

    /// `filterModels` — optional provider policy for credential-specific
    /// model availability (models.ts:111). `get_models` remains the complete
    /// synchronous catalog; the model runtime's availability refresh
    /// (`crates/rpi/src/core/model_runtime.rs` `refresh_availability_inner`)
    /// applies this filter after confirming that provider auth is
    /// configured. Default: no filter.
    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        let _ = credential;
        models
    }

    /// `filterAllModels` (models.ts:757 @ `ce8972a0e`): availability filter
    /// over every model type. The default applies [`Provider::filter_models`]
    /// to the chat models and passes the other types through (the upstream
    /// fallback).
    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        let chat: Vec<Model> = models
            .iter()
            .filter_map(|model| model.as_chat().cloned())
            .collect();
        let filtered = self.filter_models(chat, credential);
        let available: std::collections::HashSet<&str> =
            filtered.iter().map(|model| model.id.as_str()).collect();
        models
            .into_iter()
            .filter(|model| match model {
                AnyModel::Chat(chat) => available.contains(chat.id.as_str()),
                _ => true,
            })
            .collect()
    }

    /// `refreshModels?` (models.ts:104): restore the provider-scoped stored
    /// catalog and optionally fetch a newer list using the effective
    /// credential. Implementations must retain their previous list on failure
    /// and honor the shared abort signal for network requests. `None` = no
    /// dynamic overlay (upstream: the method is absent; [`Models::refresh`]
    /// skips such providers before resolving credentials).
    fn refresh_models(
        &self,
        _context: RefreshModelsContext,
    ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
        None
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String>;
    /// Fetch a provider-side deferred response when this provider supports it.
    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        None
    }

    /// Cancel a provider-side deferred response when this provider supports it.
    fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<'static, Result<(), String>>> {
        None
    }

    /// `generateImages?` (models.ts:221-226 @ abe508e1b): present when the
    /// provider supports dedicated image models. Never rejects: the
    /// implementation returns an error `AssistantImages` itself; a `None`
    /// here is wrapped by [`Models::generate_images`] as a provider error.
    fn generate_images(
        &self,
        _model: &ImageModel,
        _context: &ImagesContext,
        _options: Option<&ImagesOptions>,
    ) -> Option<BoxFuture<'static, AssistantImages>> {
        None
    }

    /// `classify?` (models.ts:228-233): present when the provider supports
    /// structured classifiers. `None` is wrapped by [`Models::classify`] as
    /// a provider error.
    fn classify(
        &self,
        _model: &ClassifierModel,
        _context: &ClassifierContext,
        _options: Option<&ClassifierOptions>,
    ) -> Option<BoxFuture<'static, ClassifierResult>> {
        None
    }

    /// The image generator registered for `api`, if any (support probe for
    /// [`Models::generate_images`] before auth resolution).
    fn image_generator(&self, _api: &str) -> Option<Arc<dyn ProviderImageGenerator>> {
        None
    }

    /// The classifier registered for `api`, if any (support probe for
    /// [`Models::classify`]).
    fn classifier(&self, _api: &str) -> Option<Arc<dyn ProviderClassifier>> {
        None
    }
}

/// `ModelsPublication` (models.ts:39-44 @ 4181f66) — the provider-owned
/// persistence/update policy handed to `RefreshModelsContext::publish`.
pub struct ModelsPublication {
    /// `None`  = leave storage unchanged.
    /// `Some(None)` = delete the stored entry.
    /// `Some(Some(entry))` = write the entry.
    pub persist: Option<Option<ModelsStoreEntry>>,
    /// Optional synchronous update of provider-private in-memory catalog
    /// state. Runs only after the persistence mutation has completed.
    pub update: Option<Box<dyn FnOnce() + Send>>,
}

/// Internal state shared between the publish handle and the `Models`
/// instance. Exposed (`pub`) so cross-crate tests (e.g. `rpi`) can construct
/// `RefreshModelsContext`s outside `rpi-ai`.
pub struct PublishShared {
    /// Provider ID (for store operations).
    pub provider_id: String,
    /// Generation captured at refresh start; the gate checks it stays current.
    pub generation: u64,
    /// Combined caller + controller signal.
    pub signal: CancellationToken,
    /// Back-reference to the `Models` store (the `Arc<dyn ModelsStore>`).
    pub store: Arc<dyn ModelsStore>,
    /// Tail of the per-provider serial publication chain (models.ts:261).
    pub chain: Arc<tokio::sync::Mutex<Option<BoxFuture<'static, ()>>>>,
    /// Current generation map (shared with `Models`).
    pub refresh_generations: Arc<RwLock<HashMap<String, u64>>>,
}

/// Handle to the generation-checked publication mechanism. Cloned into the
/// provider context and each async block that needs to publish.
#[derive(Clone)]
pub struct PublishHandle {
    pub shared: Arc<PublishShared>,
}

impl PublishHandle {
    /// `publishProviderModels` (models.ts:338-365 @ 4181f66).
    ///
    /// Returns `Ok(true)` if the publication was applied (persisted + update
    /// ran); `Ok(false)` if it was superseded or cancelled (persist may still
    /// have happened); `Err(_)` if persistence failed (update does not run).
    ///
    /// The per-provider serial publication chain is enforced by holding
    /// `chain` mutex for the entire publish body — the second concurrent
    /// publish blocks on `chain.lock()` until the first fully completes
    /// (models.ts:344-346).
    pub async fn publish(&self, publication: ModelsPublication) -> Result<bool, ModelsError> {
        let s = &self.shared;

        // Hold the chain mutex for the entire publish body so concurrent
        // publishes on the same provider serialize strictly
        // (models.ts:344-346: `previous = chain.get() ?? resolved; queued =
        // (async () => { await previous; ... })(); chain.set(tail)`).
        let _chain_guard = s.chain_lock().await;

        // Step 2 — Gate 1: signal aborted or generation mismatch.
        if s.signal.is_cancelled() || !s.generation_matches() {
            return Ok(false);
        }

        // Step 3 — persist (models.ts:349-353). Store errors propagate.
        match publication.persist {
            None => { /* leave storage unchanged */ }
            Some(None) => {
                let opts = AuthOperationOptions::with_signal(s.signal.clone());
                s.store
                    .delete(&s.provider_id, Some(&opts))
                    .await
                    .map_err(ai_error_to_models_error)?;
            }
            Some(Some(entry)) => {
                // Defensive clone (structuredClone equivalent, models.ts:352).
                let opts = AuthOperationOptions::with_signal(s.signal.clone());
                s.store
                    .write(&s.provider_id, entry.clone(), Some(&opts))
                    .await
                    .map_err(ai_error_to_models_error)?;
            }
        }

        // Step 4 — Gate 2 (models.ts:355): persist done, re-check before update.
        if s.signal.is_cancelled() || !s.generation_matches() {
            return Ok(false);
        }

        // Step 5 — run update synchronously (models.ts:356).
        if let Some(update) = publication.update {
            update();
        }
        Ok(true)
    }
}

impl PublishShared {
    fn generation_matches(&self) -> bool {
        self.refresh_generations
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.provider_id)
            .copied()
            == Some(self.generation)
    }

    /// Acquire the chain lock (serialization point for per-provider publishes).
    async fn chain_lock(&self) -> tokio::sync::MutexGuard<'_, Option<BoxFuture<'static, ()>>> {
        self.chain.lock().await
    }
}

/// `RefreshModelsContext` (models.ts:46-62 @ 4181f66).
///
/// Design note on `publish`: the upstream `context.publish(publication)` is an
/// async function returning `Promise<boolean>`. In Rust the context is moved
/// into the provider's `refresh_models` future, so the publish handle must be
/// independently cloneable. [`PublishHandle`] wraps an `Arc` of shared state
/// and is cheaply cloned.
#[derive(Clone)]
pub struct RefreshModelsContext {
    /// Effective configured credential; OAuth credentials are refreshed
    /// before network access.
    pub credential: Option<Credential>,
    /// Immutable provider-scoped catalog snapshot captured before this
    /// refresh phase. Cloned defensively so the provider can mutate freely.
    pub stored: Option<ModelsStoreEntry>,
    /// Generation-checked publication. Returns `true` if published, `false`
    /// if superseded/cancelled (persistence may still have been written).
    pub publish: PublishHandle,
    /// False during offline/cache-only initialization.
    pub allow_network: bool,
    /// Bypass provider freshness checks and fetch immediately when network
    /// access is allowed. Always `None` when `allow_network` is false.
    pub force: Option<bool>,
    /// Always present — even when the caller omits its optional signal, a
    /// concrete never-cancelled token is provided (models.ts:60-62).
    pub signal: CancellationToken,
}

impl RefreshModelsContext {
    /// Convenience accessor for providers that need to pass the context to
    /// `fetch_models` (the closure owns the context by value in upstream TS).
    pub fn clone_for_fetch(&self) -> Self {
        self.clone()
    }
}

/// `ModelsRefreshOptions` (models.ts:64-71 @ 4181f66).
#[derive(Clone, Default)]
pub struct ModelsRefreshOptions {
    /// Defaults to `true` (models.ts:387 `options.allowNetwork ?? true`).
    pub allow_network: Option<bool>,
    /// Restrict refresh to these provider IDs. Unknown and static providers
    /// are silently ignored (models.ts:391).
    pub providers: Option<Vec<String>>,
    /// Bypass provider freshness checks and fetch immediately when network
    /// access is allowed.
    pub force: Option<bool>,
    pub signal: Option<CancellationToken>,
}

/// `ModelsRefreshResult` (models.ts:73-76 @ 4181f66). Provider errors are
/// returned without rejecting; provider ids keep insertion order.
///
/// `Clone` (rpi addition, V14-13): the interactive all-catalog refresh
/// shares one in-flight future across concurrent callers
/// (`futures::future::Shared`, model-catalog-refresh.ts @ 7d8c11d37).
#[derive(Debug, Default, Clone)]
pub struct ModelsRefreshResult {
    pub aborted: bool,
    /// Provider id → error message, in provider iteration order.
    pub errors: Vec<(String, String)>,
}

/// In-flight refresh future shared by concurrent callers.
pub type InflightRefreshFuture =
    futures::future::Shared<BoxFuture<'static, Result<(), ModelsError>>>;

/// `inflightRefresh` dedup slot (models.ts:559, remote-catalog-provider.ts:50,
/// radius.ts:25): the first caller runs the refresh; concurrent callers await
/// the same in-flight future; a completed refresh is dropped from the slot so
/// the next call starts fresh (upstream `inflightRefresh ??= …` +
/// `finally { inflightRefresh = undefined }`).
pub struct InflightRefresh {
    slot: Mutex<Option<InflightRefreshFuture>>,
}

impl Default for InflightRefresh {
    fn default() -> Self {
        Self::new()
    }
}

impl InflightRefresh {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// Run `refresh` unless one is already in flight; concurrent callers join
    /// it and observe its result. Locks are held only around the take/replace
    /// of the slot (no `.await` under the lock, coding-standards §6.5).
    pub async fn join_or_run<F>(&self, refresh: F) -> Result<(), ModelsError>
    where
        F: Future<Output = Result<(), ModelsError>> + Send + 'static,
    {
        let (shared, runner) = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            match slot.take() {
                Some(shared) => {
                    // Already in flight: re-insert and join the same future.
                    *slot = Some(shared.clone());
                    (shared, false)
                }
                None => {
                    let shared = refresh.boxed().shared();
                    *slot = Some(shared.clone());
                    (shared, true)
                }
            }
        };
        let result = shared.await;
        if runner {
            // The in-flight refresh finished; drop it so a later call starts
            // fresh (upstream `finally { inflightRefresh = undefined }`).
            self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        result
    }
}

/// `mergeModels` (remote-catalog-provider.ts:12-17 @ c34f2d6ad): the
/// dynamic overlay over the baseline — same-id models replace the baseline
/// entry in place, new ids append. A map index keeps the merge linear in
/// catalog size (a per-model linear scan was quadratic).
///
/// Narrow intentional difference: upstream's `Map` collapses duplicate
/// keys inside the baseline (first position + last value), while this port
/// keeps duplicated baseline entries as-is; callers pass deduplicated
/// catalogs, and the dynamic-overlay result is identical.
pub fn merge_models(baseline: &[Model], dynamic: &[Model]) -> Vec<Model> {
    let mut merged: Vec<Model> = baseline.to_vec();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(baseline.len() + dynamic.len());
    for (position, model) in merged.iter().enumerate() {
        // `Map.set` semantics: a repeated key keeps its first position.
        index.entry(model.id.clone()).or_insert(position);
    }
    for model in dynamic {
        match index.get(&model.id) {
            Some(&position) => merged[position] = model.clone(),
            None => {
                index.insert(model.id.clone(), merged.len());
                merged.push(model.clone());
            }
        }
    }
    merged
}

/// `mergeModels` for schema v6 (`remote-catalog-provider.ts:31-35 @
/// abe508e1b`): the merge key is `type\0id`, so the same upstream id may
/// appear once per type. Same-key models replace the baseline entry in
/// place; new keys append. A map index keeps the merge linear (@ c34f2d6ad).
pub fn merge_any_models(baseline: &[AnyModel], dynamic: &[AnyModel]) -> Vec<AnyModel> {
    let key = |model: &AnyModel| format!("{}\0{}", model.model_type().as_str(), model.id());
    let mut merged: Vec<AnyModel> = baseline.to_vec();
    let mut index: HashMap<String, usize> = HashMap::with_capacity(baseline.len() + dynamic.len());
    for (position, model) in merged.iter().enumerate() {
        index.entry(key(model)).or_insert(position);
    }
    for model in dynamic {
        let model_key = key(model);
        match index.get(&model_key) {
            Some(&position) => merged[position] = model.clone(),
            None => {
                index.insert(model_key, merged.len());
                merged.push(model.clone());
            }
        }
    }
    merged
}

/// Unix epoch milliseconds (`Date.now()`).
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Store errors surface as `ModelsError("model_source", …)` on the refresh
/// path (models.ts wraps non-`Error` rejections the same way).
#[allow(dead_code)]
pub(crate) fn ai_error_to_models_error(error: AiError) -> ModelsError {
    ModelsError::new(ModelsErrorCode::ModelSource, error.to_string())
}

/// `createProvider` api input: a single implementation for all models, or a
/// map keyed by `model.api` for mixed-API providers.
#[derive(Clone)]
pub enum ProviderApi {
    Single(Arc<dyn ProviderStreams>),
    Map(HashMap<String, Arc<dyn ProviderStreams>>),
}

impl Default for ProviderApi {
    fn default() -> Self {
        ProviderApi::Map(HashMap::new())
    }
}

/// `FetchModelsFn` — the `createProvider.fetchModels` callback
/// (models.ts:750 @ 4181f66). Returns the refreshed model overlay for the
/// provider.
pub type FetchModelsFn = Arc<
    dyn Fn(RefreshModelsContext) -> BoxFuture<'static, Result<Vec<Model>, ModelsError>>
        + Send
        + Sync,
>;

/// `FilterModelsFn` — the `createProvider.filterModels` callback
/// (models.ts:751 @ 4181f66).
pub type FilterModelsFn = Arc<dyn Fn(Vec<Model>, Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// `FilterAllModelsFn` — the `createProvider.filterAllModels` callback over
/// every model type (models.ts:758 @ `ce8972a0e`).
pub type FilterAllModelsFn =
    Arc<dyn Fn(Vec<AnyModel>, Option<&Credential>) -> Vec<AnyModel> + Send + Sync>;

/// `CreateProviderOptions` (models.ts:739-754 @ 4181f66).
#[derive(Default)]
pub struct CreateProviderOptions {
    pub id: String,
    /// Display name. Default: `id`.
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<ProviderHeaders>,
    /// Required — every provider has auth semantics.
    pub auth: ProviderAuth,
    /// Static baseline model list (chat models; the legacy getter face).
    pub models: Vec<Model>,
    /// Schema-v6 all-type baseline (chat + image + classifier), in catalog
    /// order. Empty means "chat only" (the schema-v6 default for providers
    /// without image/classifier entries).
    pub all_models: Vec<AnyModel>,
    /// Classifier API implementations by api id (`classifiers`,
    /// models.ts:1023).
    pub classifiers: std::collections::HashMap<String, Arc<dyn ProviderClassifier>>,
    /// Image-generation API implementations by api id (`images`,
    /// models.ts:1021).
    pub images: std::collections::HashMap<String, Arc<dyn ProviderImageGenerator>>,
    pub api: ProviderApi,
    /// Optional dynamic catalog fetcher. When present, `createProvider`
    /// builds a `refresh_models` that restores `context.stored` then fetches
    /// (models.ts:801-826 @ 4181f66).
    pub fetch_models: Option<FetchModelsFn>,
    /// Optional model availability filter (models.ts:751).
    pub filter_models_fn: Option<FilterModelsFn>,
    /// Optional all-type availability filter (models.ts:758). When present it
    /// wins over [`Provider::filter_all_models`]'s chat fallback.
    pub filter_all_models_fn: Option<FilterAllModelsFn>,
}

struct CreatedProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    models: Vec<Model>,
    /// Schema-v6 all-type baseline (empty = derive from `models`).
    all_models: Vec<AnyModel>,
    /// Classifier API implementations by api id.
    classifiers: std::collections::HashMap<String, Arc<dyn ProviderClassifier>>,
    /// Image-generation API implementations by api id.
    images: std::collections::HashMap<String, Arc<dyn ProviderImageGenerator>>,
    api: ProviderApi,
    /// Dynamic overlay (models.ts:764-774): merged over `models` in
    /// `get_models`. Updated atomically by `refresh_models`.
    dynamic_models: Arc<Mutex<Vec<Model>>>,
    /// Optional `fetchModels` closure — when `Some`, the provider implements
    /// `refresh_models` (models.ts:801-826).
    fetch_models: Option<FetchModelsFn>,
    /// Optional `filterModels`.
    filter_models_fn: Option<FilterModelsFn>,
    /// Optional `filterAllModels`.
    filter_all_models_fn: Option<FilterAllModelsFn>,
}

impl CreatedProvider {
    fn api_for(&self, model: &Model) -> Option<&Arc<dyn ProviderStreams>> {
        match &self.api {
            ProviderApi::Single(streams) => Some(streams),
            ProviderApi::Map(by_api) => by_api.get(model.api.as_str()),
        }
    }

    fn dispatch(
        &self,
        model: &Model,
        run: impl FnOnce(&Arc<dyn ProviderStreams>) -> AssistantMessageEventStream,
    ) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => run(streams),
            None => {
                let id = self.id.clone();
                let api = model.api.clone();
                lazy_stream(model, async move {
                    Err(ModelsError::new(
                        ModelsErrorCode::Stream,
                        format!("Provider {id} has no API implementation for \"{api}\""),
                    ))
                })
            }
        }
    }
}

impl Provider for CreatedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.headers.as_ref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        let dynamic = self
            .dynamic_models
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        merge_models(&self.models, &dynamic)
    }

    /// Schema-v6 all-type read (`getAllModels?`, models.ts:174 @ abe508e1b):
    /// the static all-type baseline when the provider declared one, with the
    /// dynamic chat overlay merged on top; chat-only providers keep the
    /// legacy default.
    fn get_all_models(&self) -> Vec<AnyModel> {
        if self.all_models.is_empty() {
            return self.get_models().into_iter().map(AnyModel::Chat).collect();
        }
        let dynamic: Vec<AnyModel> = self
            .dynamic_models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .map(AnyModel::Chat)
            .collect();
        merge_any_models(&self.all_models, &dynamic)
    }

    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Option<BoxFuture<'static, AssistantImages>> {
        let implementation = self.image_generator(model.api.as_str())?;
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Some(Box::pin(async move {
            implementation
                .generate_images(&model, &context, options.as_ref())
                .await
        }))
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> Option<BoxFuture<'static, ClassifierResult>> {
        let implementation = self.classifier(model.api.as_str())?;
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Some(Box::pin(async move {
            implementation
                .classify(&model, &context, options.as_ref())
                .await
        }))
    }

    fn image_generator(&self, api: &str) -> Option<Arc<dyn ProviderImageGenerator>> {
        self.images.get(api).cloned()
    }

    fn classifier(&self, api: &str) -> Option<Arc<dyn ProviderClassifier>> {
        self.classifiers.get(api).cloned()
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        if let Some(filter) = &self.filter_all_models_fn {
            filter(models, credential)
        } else {
            let chat: Vec<Model> = models
                .iter()
                .filter_map(|model| model.as_chat().cloned())
                .collect();
            let filtered = self.filter_models(chat, credential);
            let available: std::collections::HashSet<&str> =
                filtered.iter().map(|model| model.id.as_str()).collect();
            models
                .into_iter()
                .filter(|model| match model {
                    AnyModel::Chat(chat) => available.contains(chat.id.as_str()),
                    _ => true,
                })
                .collect()
        }
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        if let Some(filter) = &self.filter_models_fn {
            filter(models, credential)
        } else {
            models
        }
    }

    /// `createProvider.refreshModels` (models.ts:801-826 @ 4181f66): restore
    /// the stored overlay via `publish`, then — when network access is allowed
    /// — fetch the dynamic catalog and publish it.
    fn refresh_models(
        &self,
        context: RefreshModelsContext,
    ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
        let fetch_models = self.fetch_models.clone()?;
        let id = self.id.clone();
        let dynamic_models = self.dynamic_models.clone();
        Some(Box::pin(async move {
            // Phase 1: restore the stored overlay (models.ts:803-816).
            if let Some(stored) = &context.stored {
                let dynamic_models = dynamic_models.clone();
                let restored: Vec<Model> = stored
                    .models
                    .iter()
                    .filter_map(AnyModel::as_chat)
                    .filter(|model| model.provider == id)
                    .cloned()
                    .collect();
                let restored_clone = restored.clone();
                let published = context
                    .publish
                    .publish(ModelsPublication {
                        persist: None,
                        update: Some(Box::new(move || {
                            *dynamic_models.lock().unwrap_or_else(|e| e.into_inner()) =
                                restored_clone;
                        })),
                    })
                    .await?;
                if !published {
                    return Ok(()); // Superseded/cancelled.
                }
            }

            // models.ts:817: if no network or aborted, stop.
            if !context.allow_network || context.signal.is_cancelled() {
                return Ok(());
            }

            // Phase 2: fetch the dynamic catalog (models.ts:818-826).
            let refreshed = match fetch_models(context.clone_for_fetch()).await {
                Ok(models) => models,
                Err(error) => return Err(error),
            };
            if context.signal.is_cancelled() {
                return Ok(());
            }
            let dynamic_models = dynamic_models.clone();
            let refreshed_clone = refreshed.clone();
            context
                .publish
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: refreshed.into_iter().map(AnyModel::Chat).collect(),
                        last_modified: None,
                        checked_at: Some(now_millis()),
                        etag: None,
                    })),
                    update: Some(Box::new(move || {
                        *dynamic_models.lock().unwrap_or_else(|e| e.into_inner()) = refreshed_clone;
                    })),
                })
                .await?;
            Ok(())
        }))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.dispatch(model, |streams| streams.stream(model, context, options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        // A model whose api has no adapter entry keeps the in-stream setup
        // error (upstream: the JS `undefined.streamSimple()` TypeError is
        // caught by `lazyStream` in `Models.streamSimple`, surfacing as an
        // error event — auth is not the failure, so the sync-Err contract
        // does not apply here).
        match self.api_for(model) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => {
                let id = self.id.clone();
                let api = model.api.clone();
                Ok(lazy_stream(model, async move {
                    Err(ModelsError::new(
                        ModelsErrorCode::Stream,
                        format!("Provider {id} has no API implementation for \"{api}\""),
                    ))
                }))
            }
        }
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.api_for(model)
            .and_then(|streams| streams.fetch_deferred(model, handle, options))
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<'static, Result<(), String>>> {
        self.api_for(model)
            .and_then(|streams| streams.cancel_deferred(model, handle, options))
    }
}

/// `createProvider`: builds a provider from parts. A single `api` streams all
/// models; an api map dispatches on `model.api`, and a model whose api has no
/// entry produces a stream error.
pub fn create_provider(input: CreateProviderOptions) -> Arc<dyn Provider> {
    Arc::new(CreatedProvider {
        name: input.name.unwrap_or_else(|| input.id.clone()),
        id: input.id,
        base_url: input.base_url,
        headers: input.headers,
        auth: input.auth,
        models: input.models,
        all_models: input.all_models,
        classifiers: input.classifiers,
        images: input.images,
        api: input.api,
        dynamic_models: Arc::new(Mutex::new(Vec::new())),
        fetch_models: input.fetch_models,
        filter_models_fn: input.filter_models_fn,
        filter_all_models_fn: input.filter_all_models_fn,
    })
}

/// `CreateModelsOptions`.
pub struct CreateModelsOptions {
    pub credentials: Option<Arc<dyn CredentialStore>>,
    pub auth_context: Option<Arc<dyn AuthContext>>,
    /// Persistent model storage for dynamic provider overlays
    /// (`modelsStore`, models.ts:198-226). Defaults to in-memory.
    pub models_store: Option<Arc<dyn ModelsStore>>,
}

/// Resolved request auth for the non-chat model runtimes (image/classifier);
/// the image/classifier counterpart of the `(Model, StreamOptions)` tuple
/// `apply_auth` returns.
struct ResolvedRequestAuth {
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    base_url: Option<String>,
}

/// `Models` — runtime collection of providers plus auth application and
/// stream convenience (T03 subset, see module docs). T21a added generation
/// guards and per-provider publication chains (models.ts:259-261 @ 4181f66).
#[derive(Clone)]
pub struct Models {
    /// Insertion-ordered like the upstream JS `Map`: `getModels()` order is
    /// observable (initial-model fallback, available-model listings).
    providers: Arc<RwLock<OrderedMap<Arc<dyn Provider>>>>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
    models_store: Arc<dyn ModelsStore>,
    /// Per-provider generation counter (models.ts:259).
    refresh_generations: Arc<RwLock<HashMap<String, u64>>>,
    /// Per-provider abort controller (models.ts:260).
    refresh_controllers: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Per-provider serial publication chain (models.ts:261).
    publication_chains: Arc<Mutex<HashMap<String, PublicationChain>>>,
}

/// Type alias for the per-provider serial publication chain tail.
type PublicationChain = Arc<tokio::sync::Mutex<Option<BoxFuture<'static, ()>>>>;

/// Drop guard for one provider refresh (models.ts:431-435 `finally`): aborts
/// the combined-signal watcher task and removes the refresh controller if it
/// is still current. Upstream relies on `finally`; here the outer `select!`
/// in [`Models::refresh`] drops the per-provider future when the caller
/// signal fires, so cleanup must run in `Drop` to not leak controllers (and
/// detached watcher tasks) from `refresh_controllers`.
struct ProviderRefreshGuard {
    models: Models,
    provider_id: String,
    controller: CancellationToken,
    watcher: tokio::task::JoinHandle<()>,
}

impl Drop for ProviderRefreshGuard {
    fn drop(&mut self) {
        self.watcher.abort();
        self.models
            .end_provider_refresh(&self.provider_id, &self.controller);
    }
}

impl Default for Models {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Models {
    /// `createModels`.
    pub fn new(options: Option<CreateModelsOptions>) -> Self {
        let options = options.unwrap_or(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: None,
        });
        Self {
            providers: Arc::new(RwLock::new(OrderedMap::default())),
            credentials: options
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            auth_context: options
                .auth_context
                .unwrap_or_else(|| Arc::new(DefaultAuthContext)),
            models_store: options
                .models_store
                .unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
            refresh_generations: Arc::new(RwLock::new(HashMap::new())),
            refresh_controllers: Arc::new(Mutex::new(HashMap::new())),
            publication_chains: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// `supersedeProviderRefresh` (models.ts:320-329 @ 4181f66).
    fn supersede_provider_refresh(&self, provider_id: &str) -> u64 {
        let generation = {
            let mut gens = self
                .refresh_generations
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let current = gens.get(provider_id).copied().unwrap_or(0) + 1;
            gens.insert(provider_id.to_owned(), current);
            current
        };
        let previous = {
            let mut controllers = self
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            controllers.remove(provider_id)
        };
        if let Some(controller) = previous {
            controller.cancel();
        }
        generation
    }

    /// `beginProviderRefresh` (models.ts:331-336).
    fn begin_provider_refresh(&self, provider_id: &str) -> (u64, CancellationToken) {
        let generation = self.supersede_provider_refresh(provider_id);
        let controller = CancellationToken::new();
        self.refresh_controllers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider_id.to_owned(), controller.clone());
        (generation, controller)
    }

    /// Remove the controller if it is still current (models.ts:432-434).
    fn end_provider_refresh(&self, provider_id: &str, controller: &CancellationToken) {
        let mut controllers = self
            .refresh_controllers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if controllers
            .get(provider_id)
            .is_some_and(|current| *current == *controller)
        {
            controllers.remove(provider_id);
        }
    }

    /// Get or create the per-provider publication chain (models.ts:261).
    fn publication_chain_for(
        &self,
        provider_id: &str,
    ) -> Arc<tokio::sync::Mutex<Option<BoxFuture<'static, ()>>>> {
        let mut chains = self
            .publication_chains
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        chains
            .entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone()
    }

    /// `setProvider` (models.ts:269-272 @ 4181f66).
    pub fn set_provider(&self, provider: Arc<dyn Provider>) {
        self.supersede_provider_refresh(provider.id());
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider.id().to_owned(), provider);
    }

    /// `deleteProvider` (models.ts:274-277 @ 4181f66).
    pub fn delete_provider(&self, id: &str) {
        self.supersede_provider_refresh(id);
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// `clearProviders` (models.ts:279-284 @ 4181f66).
    pub fn clear_providers(&self) {
        let ids: std::collections::HashSet<String> = {
            let providers = self.providers.read().unwrap_or_else(|e| e.into_inner());
            let controller_ids: std::collections::HashSet<String> = self
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned()
                .collect();
            providers.keys().cloned().chain(controller_ids).collect()
        };
        for id in ids {
            self.supersede_provider_refresh(&id);
        }
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// `getModels` — sync read of last-known models from one provider or all.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        let providers = self.providers.read().unwrap_or_else(|e| e.into_inner());
        match provider {
            Some(id) => providers
                .get(id)
                .map(|provider| provider.get_models())
                .unwrap_or_default(),
            None => providers
                .values()
                .flat_map(|provider| provider.get_models())
                .collect(),
        }
    }

    /// `getAllModels` (models.ts:446 @ abe508e1b / schema v6) — every model
    /// type from one provider or all. `get_models` stays chat-only (red
    /// line).
    pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        let providers = self.providers.read().unwrap_or_else(|e| e.into_inner());
        match provider {
            Some(id) => providers
                .get(id)
                .map(|provider| provider.get_all_models())
                .unwrap_or_default(),
            None => providers
                .values()
                .flat_map(|provider| provider.get_all_models())
                .collect(),
        }
    }

    /// `getModelsOfType` (models.ts:446-477 @ abe508e1b): every model of
    /// one type from one provider or all.
    pub fn get_models_of_type(
        &self,
        model_type: crate::types::ModelType,
        provider: Option<&str>,
    ) -> Vec<AnyModel> {
        self.get_all_models(provider)
            .into_iter()
            .filter(|model| model.model_type() == model_type)
            .collect()
    }

    /// `getModelOfType` (models.ts:446-477 @ abe508e1b): sync runtime
    /// lookup of one model of one type against last-known lists.
    pub fn get_model_of_type(
        &self,
        model_type: crate::types::ModelType,
        provider: &str,
        id: &str,
    ) -> Option<AnyModel> {
        self.get_models_of_type(model_type, Some(provider))
            .into_iter()
            .find(|model| model.id() == id)
    }

    /// `getModel` — sync runtime model lookup.
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    /// `getAuthenticatedProviders` (models.ts:678-691 @ abe508e1b): providers
    async fn get_authenticated_providers(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<(Arc<dyn Provider>, Option<Credential>)>, ModelsError> {
        let providers: Vec<Arc<dyn Provider>> = match provider_id {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let mut authenticated = Vec::new();
        for provider in providers {
            let credential = self
                .credentials
                .read(provider.id(), None)
                .await
                .map_err(|error| {
                    ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("Credential store read failed for {}", provider.id()),
                        &error.message,
                    )
                })?;
            if self.get_provider_auth(provider.id(), None).await?.is_some() {
                authenticated.push((provider, credential));
            }
        }
        Ok(authenticated)
    }

    /// `getAvailableOfType`: models backed by providers with configured auth.
    pub async fn get_available_of_type(
        &self,
        model_type: crate::types::ModelType,
        provider_id: Option<&str>,
    ) -> Result<Vec<AnyModel>, ModelsError> {
        Ok(self
            .get_all_available(provider_id)
            .await?
            .into_iter()
            .filter(|model| model.model_type() == model_type)
            .collect())
    }

    /// `getAllAvailable` (models.ts:719-735 @ abe508e1b): models of every
    /// type whose providers have complete Pi auth configuration or host-managed
    /// auth. Providers without a `filterAllModels` hook apply `filterModels` to
    /// their chat models and pass other types through (upstream fallback).
    pub async fn get_all_available(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<AnyModel>, ModelsError> {
        let providers = self.get_authenticated_providers(provider_id).await?;
        let mut available = Vec::new();
        for (provider, credential) in providers {
            let models = provider.get_all_models();
            available.extend(provider.filter_all_models(models, credential.as_ref()));
        }
        Ok(available)
    }

    /// `login` (models.ts:565-615 @ 4181f66): run the provider's auth-method
    /// login (api-key prompt or OAuth flow) outside the store lock, then write
    /// the resulting credential through the store's serialized `modify` path.
    ///
    /// `options` is the upstream `LoginOptions` slot (auth/types.ts:202-208
    /// @ abe508e1b): app-supplied context such as the stable installation ID
    /// for Sign in with ChatGPT.
    ///
    /// **Write race** (models.ts:576-608): the credential write is queued in
    /// the store's serialization lock. If the caller's signal fires **before**
    /// the mutation function starts (i.e. while still queued), the write is
    /// rejected and the credential is not stored. Once the mutation function
    /// has started (flag set), the write runs to completion regardless of
    /// cancellation.
    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: &dyn AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<Credential, ModelsError> {
        let provider = self.get_provider(provider_id).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            )
        })?;

        // If the caller already cancelled, reject immediately
        // (models.ts:567 `signal.throwIfAborted()`).
        let caller_signal = interaction.signal();
        if let Some(ref token) = caller_signal
            && token.is_cancelled()
        {
            return Err(ModelsError::aborted());
        }

        // The login network flow runs outside the store lock
        // (models.ts:574-575).
        let credential = match auth_type {
            AuthType::Oauth => {
                let Some(oauth) = provider.auth().oauth.clone() else {
                    return Err(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support oauth login", provider.name()),
                    ));
                };
                Credential::OAuth(oauth.login(interaction, options).await?)
            }
            AuthType::ApiKey => {
                let Some(api_key) = provider.auth().api_key.clone() else {
                    return Err(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                    ));
                };
                if !api_key.supports_login() {
                    return Err(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                    ));
                }
                Credential::ApiKey(api_key.login(interaction).await?)
            }
        };

        // Login write race (models.ts:581-614). The mutation function sets a
        // flag the instant it starts; a queued cancellation arriving before
        // the flag rejects without writing; once started the write completes.
        let mutation_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let credential_for_store = credential.clone();
        let mutation_started_for_fn = mutation_started.clone();

        // Pass the caller's signal into the modify options so T21b's queue
        // cancellation can reject the queued modify while it waits for the
        // preceding mutation to finish (models.ts:588 `{ signal }`).
        let store_options = caller_signal
            .as_ref()
            .map(|token| AuthOperationOptions::with_signal(token.clone()));

        // The mutation fn flips `mutation_started` to true synchronously
        // before doing anything else — this is the Rust equivalent of JS's
        // `mutationStarted = true; markMutationStarted()` running at the top
        // of the async callback (models.ts:584-585).
        let modify_future = self.credentials.modify(
            provider_id,
            Arc::new(move |_| {
                mutation_started_for_fn.store(true, std::sync::atomic::Ordering::SeqCst);
                let credential = credential_for_store.clone();
                Box::pin(async move { Ok(Some(credential)) })
            }),
            store_options.as_ref(),
        );

        let provider_id_owned = provider_id.to_owned();

        // Race the modify against the caller's cancellation signal
        // (models.ts:592-608). If the token fires while the modify is still
        // queued (mutation hasn't started), reject. Once the mutation starts
        // (flag set), we always await completion regardless of cancellation.
        //
        // When there is no caller signal, just await the modify directly.
        match caller_signal {
            Some(token) => {
                tokio::pin!(modify_future);
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        // Token fired. Check if the mutation already started.
                        if mutation_started.load(std::sync::atomic::Ordering::SeqCst) {
                            // Mutation started — it must complete (models.ts:594).
                            // We MUST re-await the modify future to ensure the
                            // store write finishes; dropping it here would
                            // cancel the in-progress write.
                            modify_future.await.map_err(|error| {
                                ModelsError::with_cause(
                                    ModelsErrorCode::Auth,
                                    format!("Credential store modify failed for {provider_id_owned}"),
                                    &error.message,
                                )
                            })?;
                            Ok(credential)
                        } else {
                            // Still queued — the store's queue cancellation
                            // (T21b) will reject the modify when it reaches
                            // the front of the queue. Credential NOT written.
                            Err(ModelsError::aborted())
                        }
                    }
                    result = &mut modify_future => {
                        match result {
                            Ok(_) => Ok(credential),
                            Err(error) => Err(ModelsError::with_cause(
                                ModelsErrorCode::Auth,
                                format!("Credential store modify failed for {provider_id_owned}"),
                                &error.message,
                            )),
                        }
                    }
                }
            }
            None => {
                // No caller signal — just await the modify.
                modify_future.await.map_err(|error| {
                    ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("Credential store modify failed for {provider_id_owned}"),
                        &error.message,
                    )
                })?;
                Ok(credential)
            }
        }
    }

    /// `logout` (models.ts:617-626 @ 4181f66): remove the stored credential.
    pub async fn logout(&self, provider_id: &str) -> Result<(), ModelsError> {
        self.credentials
            .delete(provider_id, None)
            .await
            .map_err(|error| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store delete failed for {provider_id}"),
                    &error.message,
                )
            })
    }

    /// `refresh(options)` (models.ts:386-446 @ 4181f66): two-phase refresh
    /// with generation guards and caller cancellation.
    ///
    /// Phase 1: every refreshable provider unconditionally restores its cached
    /// overlay (`allow_network=false`) **before** any auth resolution.
    /// Phase 2: if `allow_network` is true and the signal is not aborted,
    /// resolve the effective credential and fetch the latest catalog.
    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> ModelsRefreshResult {
        let options = options.unwrap_or_default();
        let allow_network = options.allow_network.unwrap_or(true);
        let caller_signal = options.signal.unwrap_or_default();

        // Already cancelled (models.ts:390).
        if caller_signal.is_cancelled() {
            return ModelsRefreshResult {
                aborted: true,
                errors: Vec::new(),
            };
        }

        let selected: Option<std::collections::HashSet<String>> = options
            .providers
            .as_ref()
            .map(|ids| ids.iter().cloned().collect());

        // Filter to refreshable + selected providers (models.ts:392-395).
        let refreshable: Vec<Arc<dyn Provider>> = self
            .get_providers()
            .into_iter()
            .filter(|provider| {
                if selected
                    .as_ref()
                    .is_some_and(|s| !s.contains(provider.id()))
                {
                    return false;
                }
                // Probe: does the provider implement refresh_models?
                let dummy = self.make_probe_context(provider.id());
                provider.refresh_models(dummy).is_some()
            })
            .collect();

        let errors = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let caller_signal_clone = caller_signal.clone();

        let refresh = async {
            let provider_futures: Vec<_> = refreshable
                .into_iter()
                .map(|provider| {
                    let this = self.clone();
                    let force = options.force;
                    let errors = errors.clone();
                    let caller_signal = caller_signal_clone.clone();
                    async move {
                        let provider_id = provider.id().to_owned();
                        let (generation, controller) = this.begin_provider_refresh(&provider_id);

                        // Combined signal: caller ∪ controller (models.ts:400).
                        let combined = caller_signal.child_token();
                        let combined_clone = combined.clone();
                        let controller_clone = controller.clone();
                        let controller_handle = tokio::spawn(async move {
                            controller_clone.cancelled().await;
                            combined_clone.cancel();
                        });
                        // Runs the models.ts:431-435 `finally` cleanup even
                        // when this future is dropped by the outer `select!`.
                        let _guard = ProviderRefreshGuard {
                            models: this.clone(),
                            provider_id: provider_id.clone(),
                            controller: controller.clone(),
                            watcher: controller_handle,
                        };

                        let operation = async {
                            // models.ts:401-418.
                            let mut credential_error: Option<ModelsError> = None;
                            let stored_credential =
                                match this.read_credential(&provider_id, &combined).await {
                                    Ok(credential) => credential,
                                    Err(error) => {
                                        credential_error = Some(error);
                                        None
                                    }
                                };

                            // Phase 1: unconditional restore.
                            this.run_provider_refresh_phase(
                                &provider,
                                stored_credential.clone(),
                                false,
                                None,
                                generation,
                                &combined,
                            )
                            .await?;

                            if let Some(error) = credential_error {
                                return Err(error);
                            }

                            if !allow_network || combined.is_cancelled() {
                                return Ok(());
                            }

                            // Phase 2: resolve credential + fetch.
                            let credential = this
                                .resolve_refresh_credential(&provider, stored_credential, &combined)
                                .await?;
                            if credential.is_none() {
                                return Ok(());
                            }
                            this.run_provider_refresh_phase(
                                &provider, credential, true, force, generation, &combined,
                            )
                            .await?;
                            Ok(())
                        };

                        // Race the operation against the combined signal
                        // (models.ts:421).
                        let outcome = tokio::select! {
                            biased;
                            _ = combined.cancelled() => {
                                Err(ModelsError::aborted())
                            }
                            result = operation => result,
                        };

                        // models.ts:422-431: errors land only when NOT aborted.
                        match outcome {
                            Ok(()) => {}
                            Err(error) => {
                                if !combined.is_cancelled() {
                                    errors
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .push((provider_id.clone(), error.message));
                                }
                            }
                        }

                        // `_guard` drops here: aborts the watcher and runs
                        // `end_provider_refresh` (models.ts:432-434).
                    }
                })
                .collect();
            futures::future::join_all(provider_futures).await;
        };

        // Race the whole refresh against the caller signal (models.ts:439-443).
        tokio::select! {
            biased;
            _ = caller_signal.cancelled() => {}
            _ = refresh => {}
        }

        let errors = errors.lock().unwrap_or_else(|e| e.into_inner()).clone();
        ModelsRefreshResult {
            aborted: caller_signal.is_cancelled(),
            errors,
        }
    }

    /// Construct a dummy probe context for the `refresh_models` check
    /// (models.ts:394).
    fn make_probe_context(&self, provider_id: &str) -> RefreshModelsContext {
        let signal = CancellationToken::new();
        let shared = Arc::new(PublishShared {
            provider_id: provider_id.to_owned(),
            generation: 0,
            signal: signal.clone(),
            store: self.models_store.clone(),
            chain: self.publication_chain_for(provider_id),
            refresh_generations: self.refresh_generations.clone(),
        });
        RefreshModelsContext {
            credential: None,
            stored: None,
            publish: PublishHandle { shared },
            allow_network: false,
            force: None,
            signal,
        }
    }

    /// `runProviderRefreshPhase` (models.ts:367-384 @ 4181f66).
    async fn run_provider_refresh_phase(
        &self,
        provider: &Arc<dyn Provider>,
        credential: Option<Credential>,
        allow_network: bool,
        force: Option<bool>,
        generation: u64,
        signal: &CancellationToken,
    ) -> Result<(), ModelsError> {
        let provider_id = provider.id().to_owned();
        let store_options = AuthOperationOptions::with_signal(signal.clone());
        // models.ts:375 does not catch: a store read failure propagates into
        // the refresh `errors` map via the caller (models.ts:422-429).
        let stored = self
            .models_store
            .read(&provider_id, Some(&store_options))
            .await
            .map_err(ai_error_to_models_error)?;

        let chain = self.publication_chain_for(&provider_id);
        let shared = Arc::new(PublishShared {
            provider_id: provider_id.clone(),
            generation,
            signal: signal.clone(),
            store: self.models_store.clone(),
            chain,
            refresh_generations: self.refresh_generations.clone(),
        });
        let context = RefreshModelsContext {
            credential,
            stored: stored.clone(),
            publish: PublishHandle { shared },
            allow_network,
            force: if allow_network { force } else { None },
            signal: signal.clone(),
        };
        if let Some(refresh) = provider.refresh_models(context) {
            refresh.await?;
        }
        Ok(())
    }

    /// `readCredential` (models.ts:477-483 @ 4181f66).
    async fn read_credential(
        &self,
        provider_id: &str,
        signal: &CancellationToken,
    ) -> Result<Option<Credential>, ModelsError> {
        let options = AuthOperationOptions::with_signal(signal.clone());
        self.credentials
            .read(provider_id, Some(&options))
            .await
            .map_err(|error| {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store read failed for {provider_id}"),
                    &error.message,
                )
            })
    }

    /// `resolveRefreshCredential` (models.ts:448-475 @ 4181f66): refresh
    /// **truly expired** OAuth tokens (now >= expires, NOT the 5-minute window)
    /// under the store lock, or resolve api-key auth.
    async fn resolve_refresh_credential(
        &self,
        provider: &Arc<dyn Provider>,
        stored: Option<Credential>,
        signal: &CancellationToken,
    ) -> Result<Option<Credential>, ModelsError> {
        if let Some(Credential::OAuth(oauth)) = &stored {
            let Some(oauth_auth) = provider.auth().oauth.clone() else {
                return Ok(None);
            };
            // Refresh path: true expiry only (models.ts:456).
            if now_millis() < oauth.expires {
                return Ok(stored);
            }
            if signal.is_cancelled() {
                return Ok(None);
            }
            // A refresh that has started survives cancellation or a
            // superseding model refresh, so a rotated refresh token is always
            // persisted (models.ts:620-636 @ `bde882c74`).
            let needs_refresh =
                Arc::new(|current: &crate::auth::OAuthCredential| now_millis() >= current.expires);
            let post = crate::auth::resolve::refresh_stored_oauth_credential(
                &self.credentials,
                provider.id(),
                oauth_auth,
                needs_refresh,
                signal,
            )
            .await?;
            return Ok(post.map(Credential::OAuth));
        }

        let Some(api_key) = provider.auth().api_key.clone() else {
            return Ok(None);
        };
        let credential = match &stored {
            Some(Credential::ApiKey(api_key_credential)) => Some(api_key_credential.clone()),
            _ => None,
        };
        let result = api_key
            .resolve(self.auth_context.as_ref(), credential.as_ref())
            .await?;
        let Some(result) = result else {
            return Ok(None);
        };
        Ok(Some(Credential::ApiKey(crate::auth::ApiKeyCredential {
            key: result.auth.api_key,
            env: result.env,
        })))
    }

    fn require_provider(&self, model: &Model) -> Result<Arc<dyn Provider>, ModelsError> {
        self.get_provider(&model.provider).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {}", model.provider),
            )
        })
    }

    /// `getAuth(providerId)` (models.ts:413-429 string arm) — provider-level
    /// auth resolution without a model (no model-header merge). Used by the
    /// llama.cpp extension's `configuredClient` (T14 W6b).
    pub async fn get_provider_auth(
        &self,
        provider_id: &str,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        resolve_provider_auth(
            provider.id(),
            provider.auth(),
            &self.credentials,
            &self.auth_context,
            overrides,
        )
        .await
    }

    /// `getAuth` — resolves provider auth plus static model headers.
    pub async fn get_auth(
        &self,
        model: &Model,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        self.get_auth_for_headers(&model.provider, model.headers.as_ref(), overrides)
            .await
    }

    /// `getAuth` for a non-chat model (image/classifier entries carry the
    /// same optional static `headers` map).
    pub async fn get_auth_for_headers(
        &self,
        provider_id: &str,
        model_headers: Option<&std::collections::BTreeMap<String, String>>,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        let result = resolve_provider_auth(
            provider.id(),
            provider.auth(),
            &self.credentials,
            &self.auth_context,
            overrides,
        )
        .await?;
        let Some(mut result) = result else {
            return Ok(None);
        };
        if let Some(model_headers) = model_headers {
            let model_headers: ProviderHeaders = model_headers
                .iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect();
            result.auth.headers = merge_headers(result.auth.headers.as_ref(), Some(&model_headers));
        }
        Ok(Some(result))
    }

    /// `applyAuth`: resolves auth, merges headers (`transformHeaders` runs
    /// last on the fully assembled headers, for all authenticated requests),
    /// env and baseUrl into the request model/options. The transform is
    /// stripped from the final options before provider dispatch
    /// (models.ts:636-665 @ 4181f66).
    async fn apply_auth(
        &self,
        model: &Model,
        stream_options: Option<&StreamOptions>,
        transform_headers: Option<&ModelsRequestTransforms>,
    ) -> Result<(Model, Option<StreamOptions>), ModelsError> {
        self.require_provider(model)?;
        let resolution = self
            .get_auth(
                model,
                Some(&AuthResolutionOverrides {
                    api_key: stream_options.and_then(|o| o.api_key.clone()),
                    env: stream_options.and_then(|o| o.env.clone()),
                    ..Default::default()
                }),
            )
            .await?;
        let Some(resolution) = resolution else {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Provider is not configured: {}", model.provider),
            ));
        };
        let auth = resolution.auth;

        // Explicit request options win per-field; the Models-only transform
        // runs last.
        let api_key = stream_options
            .and_then(|o| o.api_key.clone())
            .or(auth.api_key);
        let mut headers = merge_headers(
            auth.headers.as_ref(),
            stream_options.and_then(|o| o.headers.as_ref()),
        );
        if let Some(transform) = transform_headers {
            headers = Some(transform(headers.unwrap_or_default()).await);
        }
        let env = match (resolution.env, stream_options.and_then(|o| o.env.clone())) {
            (None, None) => None,
            (a, b) => {
                let mut merged = a.unwrap_or_default();
                merged.extend(b.unwrap_or_default());
                Some(merged)
            }
        };
        let request_model = match auth.base_url {
            Some(base_url) => Model {
                base_url,
                ..model.clone()
            },
            None => model.clone(),
        };
        // Upstream always builds an options object (`{ ...providerOptions,
        // apiKey, headers, env }`), even when the caller passed none.
        let request_options = {
            let mut options = stream_options.cloned().unwrap_or_default();
            options.api_key = api_key;
            options.headers = headers;
            options.env = env;
            Some(options)
        };

        Ok((request_model, request_options))
    }

    /// Resolved request auth for a non-chat model (image/classifier): the
    /// four-level credential resolution (stored credentials / OAuth / runtime
    /// API keys / `models.json` headers) plus request-option overrides.
    async fn resolve_request_auth(
        &self,
        provider_id: &str,
        model_headers: Option<&std::collections::BTreeMap<String, String>>,
        api_key: Option<String>,
        headers: Option<ProviderHeaders>,
        env: Option<ProviderEnv>,
    ) -> Result<ResolvedRequestAuth, ModelsError> {
        if self.get_provider(provider_id).is_none() {
            return Err(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            ));
        }
        let resolution = self
            .get_auth_for_headers(
                provider_id,
                model_headers,
                Some(&AuthResolutionOverrides {
                    api_key: api_key.clone(),
                    env: env.clone(),
                    ..Default::default()
                }),
            )
            .await?;
        let Some(resolution) = resolution else {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Provider is not configured: {provider_id}"),
            ));
        };
        let auth = resolution.auth;
        let env = match (resolution.env, env) {
            (None, None) => None,
            (a, b) => {
                let mut merged = a.unwrap_or_default();
                merged.extend(b.unwrap_or_default());
                Some(merged)
            }
        };
        Ok(ResolvedRequestAuth {
            api_key: api_key.or(auth.api_key),
            headers: merge_headers(auth.headers.as_ref(), headers.as_ref()),
            env,
            base_url: auth.base_url,
        })
    }

    /// `generateImages` (models.ts:948-965 @ abe508e1b): resolve the model
    /// through its owning provider with request-time authentication. Never
    /// rejects: unknown providers, unconfigured auth, and providers without
    /// an image implementation return an error `AssistantImages`.
    pub async fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(CancellationToken::is_cancelled);
        match self.generate_images_inner(model, context, options).await {
            Ok(result) => result,
            Err(error) => image_error_result(model, &error, aborted),
        }
    }

    async fn generate_images_inner(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Result<AssistantImages, String> {
        let provider = self
            .get_provider(&model.provider)
            .ok_or_else(|| format!("Unknown provider: {}", model.provider))?;
        let implementation = provider
            .image_generator(model.api.as_str())
            .ok_or_else(|| {
                format!(
                    "Provider {} has no image generation implementation for \"{}\"",
                    model.provider, model.api
                )
            })?;
        let auth = self
            .resolve_request_auth(
                &model.provider,
                model.headers.as_ref(),
                options.and_then(|options| options.api_key.clone()),
                options.and_then(|options| options.headers.clone()),
                options.and_then(|options| options.env.clone()),
            )
            .await
            .map_err(|error| error.message.clone())?;
        let request_model = match auth.base_url {
            Some(base_url) => ImageModel {
                base_url,
                ..model.clone()
            },
            None => model.clone(),
        };
        let request_options = {
            let mut options = options.cloned().unwrap_or_default();
            options.api_key = auth.api_key;
            options.headers = auth.headers;
            options.env = auth.env;
            options
        };
        Ok(implementation
            .generate_images(&request_model, context, Some(&request_options))
            .await)
    }

    /// `classify` (models.ts:966-983 @ abe508e1b): resolve the classifier
    /// through its owning provider with request-time authentication. Never
    /// rejects.
    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(CancellationToken::is_cancelled);
        if let Err(error) = crate::types::assert_classifier_input_supported(model, context) {
            return classifier_error_result(model, &error.message, aborted);
        }
        match self.classify_inner(model, context, options).await {
            Ok(result) => result,
            Err(error) => classifier_error_result(model, &error, aborted),
        }
    }

    async fn classify_inner(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> Result<ClassifierResult, String> {
        let provider = self
            .get_provider(&model.provider)
            .ok_or_else(|| format!("Unknown provider: {}", model.provider))?;
        let implementation = provider.classifier(model.api.as_str()).ok_or_else(|| {
            format!(
                "Provider {} has no classifier implementation for \"{}\"",
                model.provider, model.api
            )
        })?;
        let auth = self
            .resolve_request_auth(
                &model.provider,
                model.headers.as_ref(),
                options.and_then(|options| options.api_key.clone()),
                options.and_then(|options| options.headers.clone()),
                options.and_then(|options| options.env.clone()),
            )
            .await
            .map_err(|error| error.message.clone())?;
        let request_model = match auth.base_url {
            Some(base_url) => ClassifierModel {
                base_url,
                ..model.clone()
            },
            None => model.clone(),
        };
        let request_options = {
            let mut options = options.cloned().unwrap_or_default();
            options.api_key = auth.api_key;
            options.headers = auth.headers;
            options.env = auth.env;
            options
        };
        Ok(implementation
            .classify(&request_model, context, Some(&request_options))
            .await)
    }

    /// `stream` — resolves auth lazily behind the returned stream, then
    /// delegates to the owning provider. The caller's [`Context`] is
    /// normalized into a [`TranscriptContext`] before any provider sees it
    /// (models.ts:684, #9548): the prompt and tool declarations ride the
    /// transcript's system messages.
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsStreamOptions>,
    ) -> AssistantMessageEventStream {
        let this = self.clone();
        let model = model.clone();
        let transcript = crate::utils::transcript::normalize_context(context);
        lazy_stream(&model.clone(), async move {
            let provider = this.require_provider(&model)?;
            let (request_model, request_options) = this
                .apply_auth(
                    &model,
                    options.as_ref().map(|o| &o.stream),
                    options.as_ref().and_then(|o| o.transform_headers.as_ref()),
                )
                .await?;
            Ok(provider.stream(&request_model, &transcript, request_options))
        })
    }

    /// `complete` — `stream(...).result()`.
    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsStreamOptions>,
    ) -> Option<crate::types::AssistantMessage> {
        self.stream(model, context, options).result().await
    }

    /// `streamSimple`. Normalizes the caller's [`Context`] before dispatch
    /// (models.ts:704, #9548).
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let this = self.clone();
        let model = model.clone();
        let transcript = crate::utils::transcript::normalize_context(context);
        lazy_stream(&model.clone(), async move {
            let provider = this.require_provider(&model)?;
            let (request_model, request_options) = this
                .apply_auth(
                    &model,
                    options.as_ref().map(|o| &o.simple.stream),
                    options.as_ref().and_then(|o| o.transform_headers.as_ref()),
                )
                .await?;
            let simple_options = request_options.map(|stream| SimpleStreamOptions {
                stream,
                reasoning: options.as_ref().and_then(|o| o.simple.reasoning),
                thinking_budgets: options
                    .as_ref()
                    .and_then(|o| o.simple.thinking_budgets.clone()),
                tool_choice: options.as_ref().and_then(|o| o.simple.tool_choice),
            });
            provider
                .stream_simple(&request_model, &transcript, simple_options)
                .map_err(|message| {
                    // Upstream `lazyStream` catch: the adapter's synchronous
                    // auth throw surfaces as an in-stream setup error event
                    // at this layer (8b5899dce; lazy.ts:55-60).
                    ModelsError::new(ModelsErrorCode::Stream, message)
                })
        })
    }

    /// `completeSimple`.
    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> Option<crate::types::AssistantMessage> {
        self.stream_simple(model, context, options).result().await
    }
    /// Fetch a provider-side deferred response through the owning provider.
    pub fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        let this = self.clone();
        let model = model.clone();
        let handle = handle.clone();
        lazy_stream(&model.clone(), async move {
            let provider = this.require_provider(&model)?;
            let deferred = options.as_ref().map(|options| &options.deferred);
            let stream_options = deferred.map(|options| StreamOptions {
                request: options.request.clone(),
                ..Default::default()
            });
            let (request_model, request_options) = this
                .apply_auth(
                    &model,
                    stream_options.as_ref(),
                    options
                        .as_ref()
                        .and_then(|options| options.transform_headers.as_ref()),
                )
                .await?;
            let mut fetch_options = deferred.cloned().unwrap_or_default();
            fetch_options.request = request_options
                .map(|options| options.request)
                .unwrap_or_default();
            provider
                .fetch_deferred(&request_model, &handle, Some(fetch_options))
                .ok_or_else(|| {
                    ModelsError::new(
                        ModelsErrorCode::Provider,
                        format!(
                            "Provider {} does not support deferred responses",
                            model.provider
                        ),
                    )
                })
        })
    }

    /// Fetch one deferred response result.
    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> Option<crate::types::AssistantMessage> {
        self.stream_deferred(model, handle, options).result().await
    }

    /// Cancel a provider-side deferred response.
    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredCancelOptions>,
    ) -> Result<(), ModelsError> {
        let provider = self.require_provider(model)?;
        let deferred = options.as_ref().map(|options| &options.deferred);
        let stream_options = deferred.map(|options| StreamOptions {
            request: options.request.clone(),
            ..Default::default()
        });
        let (request_model, request_options) = self
            .apply_auth(
                model,
                stream_options.as_ref(),
                options
                    .as_ref()
                    .and_then(|options| options.transform_headers.as_ref()),
            )
            .await?;
        let mut cancel_options = deferred.cloned().unwrap_or_default();
        cancel_options.request = request_options
            .map(|options| options.request)
            .unwrap_or_default();
        let cancel = provider
            .cancel_deferred(&request_model, handle, Some(cancel_options))
            .ok_or_else(|| {
                ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!(
                        "Provider {} does not support deferred responses",
                        model.provider
                    ),
                )
            })?;
        cancel
            .await
            .map_err(|error| ModelsError::new(ModelsErrorCode::Provider, error))
    }
}

/// `assertChatModel` (utils/model-operations.ts:26-30 @ abe508e1b): reject
/// non-chat models at the chat stream entry with a `ModelsError`.
pub fn assert_chat_model(model: &AnyModel) -> Result<&Model, ModelsError> {
    match model {
        AnyModel::Chat(chat) => Ok(chat),
        other => Err(ModelsError::new(
            ModelsErrorCode::Provider,
            format!(
                "Model {}/{} is not a chat model",
                other.provider(),
                other.id()
            ),
        )),
    }
}

/// `createModels`.
pub fn create_models(options: Option<CreateModelsOptions>) -> Models {
    Models::new(options)
}

/// `ProviderModelsStore` scoped to one provider id (models.ts:287-291) —
/// providers cannot access other providers' catalogs.
pub struct ScopedModelsStore {
    store: Arc<dyn ModelsStore>,
    provider_id: String,
}

impl ScopedModelsStore {
    pub fn new(store: Arc<dyn ModelsStore>, provider_id: impl Into<String>) -> Self {
        Self {
            store,
            provider_id: provider_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl ProviderModelsStore for ScopedModelsStore {
    async fn read(
        &self,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        self.store.read(&self.provider_id, options).await
    }

    async fn write(
        &self,
        entry: ModelsStoreEntry,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError> {
        self.store.write(&self.provider_id, entry, options).await
    }

    async fn delete(&self, options: Option<&AuthOperationOptions>) -> Result<(), AiError> {
        self.store.delete(&self.provider_id, options).await
    }
}

const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// `getSupportedThinkingLevels`: `["off"]` for non-reasoning models; levels
/// mapped to `null` are unsupported; xhigh/max require an explicit mapping.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }

    EXTENDED_THINKING_LEVELS
        .into_iter()
        .filter(|level| {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level));
            match mapped {
                // JSON null: level unsupported.
                Some(None) => false,
                // xhigh/max need an explicit mapped value.
                Some(Some(_)) => true,
                None => !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max),
            }
        })
        .collect()
}

/// `clampThinkingLevel`: find the nearest available level, looking up first,
/// then down.
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available_levels = get_supported_thinking_levels(model);
    if available_levels.contains(&level) {
        return level;
    }

    let Some(requested_index) = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level) else {
        return available_levels
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
    };

    for candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available_levels.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available_levels.contains(candidate) {
            return *candidate;
        }
    }
    available_levels
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

/// `modelsAreEqual`.
pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::auth::{ApiKeyAuth, ApiKeyCredential, AuthEvent, AuthPrompt, ModelAuth};
    use crate::types::{
        ApiKind, DoneReason, ModelThinkingLevel, ModelType, StopReason, StreamEvent, Usage,
    };

    fn model(provider: &str, api: &str) -> Model {
        serde_json::from_value(json!({
            "id": "m", "name": "m", "api": api, "provider": provider,
            "baseUrl": "https://example.com", "reasoning": false, "input": ["text"],
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model")
    }

    /// Model with a caller-chosen id (the generic [`model`] helper hardcodes
    /// `"m"`).
    fn model_with_id(provider: &str, id: &str) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": id, "api": "anthropic-messages", "provider": provider,
            "baseUrl": "https://example.com", "reasoning": false, "input": ["text"],
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model")
    }

    struct StaticKeyAuth;

    #[async_trait::async_trait]
    impl ApiKeyAuth for StaticKeyAuth {
        fn name(&self) -> &str {
            "Test API key"
        }

        async fn resolve(
            &self,
            _ctx: &dyn AuthContext,
            _credential: Option<&crate::auth::ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("sk-test".to_owned()),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: Some("TEST_API_KEY".to_owned()),
            }))
        }
    }

    struct EchoStreams;

    impl ProviderStreams for EchoStreams {
        fn stream(
            &self,
            model: &Model,
            _context: &crate::types::TranscriptContext,
            options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            let stream = AssistantMessageEventStream::new();
            let mut partial = crate::types::AssistantMessage {
                role: crate::types::AssistantRole::Assistant,
                content: vec![],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Pending,
                error_message: None,
                timestamp: 0,
                deferred: None,
                end_turn: None,
                raw_stop_reason: None,
                duration_ms: None,
            };
            stream.push(StreamEvent::Start {
                partial: Arc::new(partial.clone()),
            });
            // Surface the resolved api key in the final message so tests can
            // assert auth application happened.
            partial.stop_reason = StopReason::Stop;
            partial.error_message = options.and_then(|o| o.request.api_key);
            stream.push(StreamEvent::Done {
                reason: DoneReason::Stop,
                message: partial,
            });
            stream.end(None);
            stream
        }

        fn stream_simple(
            &self,
            model: &Model,
            context: &crate::types::TranscriptContext,
            options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            Ok(self.stream(model, context, options.map(|o| o.stream)))
        }
    }

    fn test_provider(id: &str, api: ProviderApi) -> Arc<dyn Provider> {
        create_provider(CreateProviderOptions {
            id: id.to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            models: vec![model(id, ApiKind::ANTHROPIC_MESSAGES)],
            api,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_models_stream_applies_auth() {
        let models = Models::new(None);
        models.set_provider(test_provider(
            "test",
            ProviderApi::Single(Arc::new(EchoStreams)),
        ));
        let model = models.get_model("test", "m").expect("model");
        let result = models
            .complete(&model, &Context::default(), None)
            .await
            .expect("result");
        assert_eq!(result.error_message, Some("sk-test".to_owned()));
    }

    #[tokio::test]

    async fn test_deferred_unsupported_provider_returns_error_and_does_not_cancel() {
        let models = Models::new(None);
        models.set_provider(test_provider(
            "test",
            ProviderApi::Single(Arc::new(EchoStreams)),
        ));
        let model = models.get_model("test", "m").expect("model");
        let handle = DeferredHandle {
            provider: "test".to_owned(),
            model_id: model.id.clone(),
            api: model.api.as_str().to_owned(),
            id: "deferred-id".to_owned(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        let fetched = models
            .fetch_deferred(&model, &handle, None)
            .await
            .expect("unsupported fetch is represented by an error message");
        assert_eq!(fetched.stop_reason, crate::types::StopReason::Error);
        assert_eq!(
            fetched.error_message.as_deref(),
            Some("Provider test does not support deferred responses")
        );
        let error = models
            .cancel_deferred(&model, &handle, None)
            .await
            .expect_err("unsupported cancellation must fail");
        assert_eq!(
            error.message,
            "Provider test does not support deferred responses"
        );
    }
    #[tokio::test]
    async fn test_models_stream_unknown_provider_stream_error() {
        let models = Models::new(None);
        let model = model("ghost", ApiKind::ANTHROPIC_MESSAGES);
        let events: Vec<StreamEvent> =
            futures::StreamExt::collect(models.stream(&model, &Context::default(), None)).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Error { error, .. } => {
                assert_eq!(error.stop_reason, StopReason::Error);
                assert_eq!(
                    error.error_message,
                    Some("Unknown provider: ghost".to_owned())
                );
            }
            other => panic!("expected error event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_provider_missing_api_stream_error() {
        let models = Models::new(None);
        // Provider only implements openai-completions; the model asks for
        // anthropic-messages.
        let mut map = HashMap::new();
        map.insert(
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(EchoStreams) as Arc<dyn ProviderStreams>,
        );
        models.set_provider(test_provider("test", ProviderApi::Map(map)));
        let model = models.get_model("test", "m").expect("model");
        let events: Vec<StreamEvent> =
            futures::StreamExt::collect(models.stream(&model, &Context::default(), None)).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Error { error, .. } => assert_eq!(
                error.error_message,
                Some(
                    "Provider test has no API implementation for \"anthropic-messages\"".to_owned()
                )
            ),
            other => panic!("expected error event, got {other:?}"),
        }
    }

    /// V14-05 FR-H R1 fixture (runtime map change itself is V14-07 FR-C):
    /// the openrouter dual-API shape — `anthropic-messages` and
    /// `openai-completions` entries side by side, dispatch by `model.api`
    /// (upstream providers/openrouter.ts:8-27). Old sessions whose Claude
    /// messages were recorded under `openai-completions` keep that routing
    /// surface; the Anthropic-side behavior (managed gating / replay) is
    /// covered by the `mid_convo_effort_tests` module.
    #[tokio::test]
    async fn test_create_provider_openrouter_dual_api_map_dispatch() {
        let models = Models::new(None);
        let mut map = HashMap::new();
        map.insert(
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            Arc::new(EchoStreams) as Arc<dyn ProviderStreams>,
        );
        map.insert(
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(EchoStreams) as Arc<dyn ProviderStreams>,
        );
        models.set_provider(test_provider("openrouter", ProviderApi::Map(map)));

        // anthropic/claude-* catalog entry: api = anthropic-messages.
        let anthropic_model = models.get_model("openrouter", "m").expect("model");
        assert_eq!(anthropic_model.api, ApiKind::from("anthropic-messages"));
        let result = models
            .complete(&anthropic_model, &Context::default(), None)
            .await
            .expect("result");
        assert_eq!(result.stop_reason, StopReason::Stop);

        // Same provider, legacy openai-completions model: the other entry.
        let legacy = models.get_model("openrouter", "m").expect("model");
        let _ = legacy;
    }

    #[tokio::test]
    async fn test_create_provider_api_map_dispatch() {
        let models = Models::new(None);
        let mut map = HashMap::new();
        map.insert(
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            Arc::new(EchoStreams) as Arc<dyn ProviderStreams>,
        );
        models.set_provider(test_provider("test", ProviderApi::Map(map)));
        let model = models.get_model("test", "m").expect("model");
        let result = models
            .complete(&model, &Context::default(), None)
            .await
            .expect("result");
        assert_eq!(result.stop_reason, StopReason::Stop);
    }

    // ------------------------------------------------------------------
    // login / logout (models.ts:431-453)
    // ------------------------------------------------------------------

    /// Records the prompts it receives and answers with a fixed string.
    struct RecordingInteraction {
        prompts: Mutex<Vec<AuthPrompt>>,
        answer: String,
    }

    impl RecordingInteraction {
        fn with_answer(answer: &str) -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
                answer: answer.to_owned(),
            }
        }

        fn prompts(&self) -> Vec<AuthPrompt> {
            self.prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl AuthInteraction for RecordingInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> crate::auth::types::BoxFutureSend<'a, Result<String, ModelsError>> {
            Box::pin(async move {
                self.prompts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(prompt);
                Ok(self.answer.clone())
            })
        }

        fn notify(&self, _event: AuthEvent) {}
    }

    /// api-key auth with a working `login` (upstream `envApiKeyAuth` /
    /// `anthropicApiKeyAuth` shape): prompts for the key, records the prompt.
    struct PromptLoginAuth {
        prompts: Mutex<Vec<AuthPrompt>>,
    }

    impl PromptLoginAuth {
        fn new() -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn prompts(&self) -> Vec<AuthPrompt> {
            self.prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl ApiKeyAuth for PromptLoginAuth {
        fn name(&self) -> &str {
            "Test API key"
        }

        fn supports_login(&self) -> bool {
            true
        }

        async fn login(
            &self,
            interaction: &dyn AuthInteraction,
        ) -> Result<crate::auth::ApiKeyCredential, ModelsError> {
            let key = interaction
                .prompt(AuthPrompt::secret("Enter Test API key"))
                .await?;
            self.prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(AuthPrompt::secret("Enter Test API key"));
            Ok(crate::auth::ApiKeyCredential {
                key: Some(key),
                env: None,
            })
        }

        async fn resolve(
            &self,
            _ctx: &dyn AuthContext,
            _credential: Option<&crate::auth::ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            Ok(None)
        }
    }

    /// OAuth auth with a working `login` (used to pin the OAuth branch shape;
    /// the interactive dialog runs it through `ModelRuntime::login`).
    struct PromptLoginOAuth;

    #[async_trait::async_trait]
    impl crate::auth::OAuthAuth for PromptLoginOAuth {
        fn name(&self) -> &str {
            "Test OAuth"
        }

        async fn login(
            &self,
            interaction: &dyn AuthInteraction,
            _options: Option<&LoginOptions>,
        ) -> Result<crate::auth::OAuthCredential, ModelsError> {
            let _ = interaction.prompt(AuthPrompt::secret("Authorize")).await?;
            Ok(crate::auth::OAuthCredential {
                refresh: "refresh-token".to_owned(),
                access: "access-token".to_owned(),
                expires: 0,
                extra: Map::new(),
            })
        }

        async fn refresh(
            &self,
            credential: &crate::auth::OAuthCredential,
            _signal: Option<&tokio_util::sync::CancellationToken>,
        ) -> Result<crate::auth::OAuthCredential, ModelsError> {
            Ok(credential.clone())
        }

        async fn to_auth(
            &self,
            credential: &crate::auth::OAuthCredential,
        ) -> Result<ModelAuth, ModelsError> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                headers: None,
                base_url: None,
            })
        }
    }

    fn models_with_store() -> (Models, Arc<InMemoryCredentialStore>) {
        let store = Arc::new(InMemoryCredentialStore::new());
        let models = Models::new(Some(CreateModelsOptions {
            credentials: Some(store.clone()),
            auth_context: None,
            models_store: None,
        }));
        (models, store)
    }

    /// api_key login runs the method's `login` with the interaction, writes
    /// the credential through the store, and returns it (models.ts:438-444).
    #[tokio::test]
    async fn test_models_login_api_key_writes_credential() {
        let (models, store) = models_with_store();
        let api_key = Arc::new(PromptLoginAuth::new());
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(api_key.clone()),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));
        let interaction = RecordingInteraction::with_answer("entered-key");

        let credential = models
            .login("test", AuthType::ApiKey, &interaction, None)
            .await
            .expect("login");

        assert_eq!(
            credential,
            Credential::ApiKey(ApiKeyCredential {
                key: Some("entered-key".to_owned()),
                env: None,
            })
        );
        assert_eq!(
            store.read("test", None).await.expect("read"),
            Some(credential),
            "login result must be persisted"
        );
        assert_eq!(api_key.prompts().len(), 1);
        assert!(matches!(
            api_key.prompts()[0],
            AuthPrompt::Secret { ref message, .. } if message == "Enter Test API key"
        ));
    }

    /// OAuth login runs the oauth method's `login` and persists the OAuth
    /// credential (models.ts:434-444); the interactive OAuth dialog consumes
    /// this path through `ModelRuntime::login`.
    #[tokio::test]
    async fn test_models_login_oauth_writes_credential() {
        let (models, store) = models_with_store();
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(PromptLoginOAuth)),
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));
        let interaction = RecordingInteraction::with_answer("authorized");

        let credential = models
            .login("test", AuthType::Oauth, &interaction, None)
            .await
            .expect("login");

        match credential {
            Credential::OAuth(ref oauth) => {
                assert_eq!(oauth.access, "access-token");
                assert_eq!(oauth.refresh, "refresh-token");
            }
            other => panic!("expected oauth credential, got {other:?}"),
        }
        assert_eq!(
            store.read("test", None).await.expect("read"),
            Some(credential),
            "login result must be persisted"
        );
    }

    #[tokio::test]
    async fn test_models_login_unknown_provider_errors() {
        let (models, _store) = models_with_store();
        let error = models
            .login(
                "ghost",
                AuthType::ApiKey,
                &RecordingInteraction::with_answer("k"),
                None,
            )
            .await
            .expect_err("unknown provider must error");
        assert_eq!(error.code, ModelsErrorCode::Provider);
        assert_eq!(error.message, "Unknown provider: ghost");
    }

    /// Ambient-only providers (no `login` upstream) error with the upstream
    /// message before any prompt is shown (models.ts:435-437).
    #[tokio::test]
    async fn test_models_login_ambient_only_provider_errors() {
        let (models, _store) = models_with_store();
        // StaticKeyAuth implements resolve but not login.
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: Some("Test provider".to_owned()),
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));
        let interaction = RecordingInteraction::with_answer("k");
        let error = models
            .login("test", AuthType::ApiKey, &interaction, None)
            .await
            .expect_err("ambient-only provider must error");
        assert_eq!(error.code, ModelsErrorCode::Auth);
        assert_eq!(
            error.message,
            "Test provider does not support api_key login"
        );
        assert!(
            interaction.prompts().is_empty(),
            "no prompt may be shown before the capability check"
        );
    }

    /// A provider missing the requested auth method errors with the upstream
    /// message (`method` is undefined, models.ts:434-437).
    #[tokio::test]
    async fn test_models_login_missing_auth_method_errors() {
        let (models, _store) = models_with_store();
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: Some("Test provider".to_owned()),
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(PromptLoginOAuth)),
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));
        let interaction = RecordingInteraction::with_answer("k");
        let error = models
            .login("test", AuthType::ApiKey, &interaction, None)
            .await
            .expect_err("missing api_key method must error");
        assert_eq!(error.code, ModelsErrorCode::Auth);
        assert_eq!(
            error.message,
            "Test provider does not support api_key login"
        );
        assert!(interaction.prompts().is_empty());

        let interaction = RecordingInteraction::with_answer("k");
        let error = models
            .login("test", AuthType::Oauth, &interaction, None)
            .await
            .expect("oauth login");
        assert!(matches!(error, Credential::OAuth(_)));
    }

    #[tokio::test]
    async fn test_models_logout_deletes_the_stored_credential() {
        let (models, store) = models_with_store();
        store
            .modify(
                "test",
                Arc::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("stored-key".to_owned()),
                            env: None,
                        })))
                    })
                }),
                None,
            )
            .await
            .expect("seed");

        models.logout("test").await.expect("logout");
        assert_eq!(store.read("test", None).await.expect("read"), None);
        // Logging out without a credential is a no-op (models.ts:447-453).
        models.logout("test").await.expect("logout again");
    }

    fn thinking_model(map: Option<serde_json::Value>) -> Model {
        let mut value = json!({
            "id": "m", "name": "m", "api": "anthropic-messages", "provider": "p",
            "baseUrl": "https://example.com", "reasoning": true, "input": ["text"],
            "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
            "contextWindow": 1000, "maxTokens": 100
        });
        if let Some(map) = map {
            value["thinkingLevelMap"] = map;
        }
        serde_json::from_value(value).expect("model")
    }

    #[test]
    fn test_get_supported_thinking_levels() {
        let model = thinking_model(None);
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
            ]
        );

        // null marks unsupported; explicit value enables xhigh.
        let model = thinking_model(Some(json!({"minimal": null, "xhigh": "xhigh"})));
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
            ]
        );

        // Non-reasoning models only support off.
        let mut model = thinking_model(None);
        model.reasoning = false;
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![ModelThinkingLevel::Off]
        );
    }

    /// #9485 (bb0f4aa60): provider-supplied DeepSeek V4.1 effort metadata is
    /// preserved through relays — the map the generator bakes from
    /// models.dev reasoning options (not the generic deepseek-v4 default)
    /// drives the supported levels. Fixture mirrors of upstream
    /// `supports-xhigh.test.ts` "preserves low/high/max metadata for DeepSeek
    /// V4.1 Flash on OpenRouter / opencode-go" (the catalog data lands with
    /// the V15-03 regen; the level-derivation behavior is adapter-side).
    #[test]
    fn test_get_supported_thinking_levels_deepseek_v41_flash_effort_metadata() {
        // OpenRouter deepseek/deepseek-v4.1-flash: models.dev effort metadata
        // (off/low/high/max) — preserved instead of the generic deepseek-v4
        // default map (which would lose low and max).
        let model = thinking_model(Some(json!({
            "off": "none", "minimal": null, "low": "low",
            "medium": null, "high": "high", "xhigh": null, "max": "max"
        })));
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ]
        );

        // opencode-go deepseek-v4.1-flash: low/high/max only (no off).
        let model = thinking_model(Some(json!({
            "off": null, "minimal": null, "low": "low", "medium": null,
            "high": "high", "xhigh": null, "max": "max"
        })));
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ]
        );
    }

    #[test]
    fn test_clamp_thinking_level_up_first_then_down() {
        // low unsupported: clamp up to medium (not down to minimal).
        let model = thinking_model(Some(json!({"low": null})));
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Low),
            ModelThinkingLevel::Medium
        );
        // max unsupported and nothing above: clamp down to high.
        let model = thinking_model(None);
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Max),
            ModelThinkingLevel::High
        );
        // Non-reasoning: everything clamps to off.
        let mut model = thinking_model(None);
        model.reasoning = false;
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );
    }

    // ------------------------------------------------------------------
    // refresh surface (models.ts:276-328; W6-C)
    // ------------------------------------------------------------------

    #[test]
    fn test_merge_models_overlays_dynamic_by_id() {
        let baseline = vec![model_with_id("p", "a"), model_with_id("p", "b")];
        let dynamic = vec![model_with_id("p", "b"), model_with_id("p", "c")];
        let merged = merge_models(&baseline, &dynamic);
        assert_eq!(
            merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        // Same-id dynamic entries replace the baseline entry in place.
        assert_eq!(merged[1].id, "b");
        assert_eq!(merged[1].provider, "p");
    }

    #[test]
    fn test_merge_any_models_keys_by_type_and_id() {
        // Schema v6: the same upstream id may appear once per type
        // (`type\0id` merge key).
        let chat = AnyModel::Chat(model_with_id("p", "same"));
        let image: AnyModel = serde_json::from_value(serde_json::json!({
            "type": "image",
            "id": "same",
            "name": "Image Same",
            "api": "openrouter-images",
            "provider": "p",
            "baseUrl": "https://example.test",
            "input": ["text"],
            "output": ["image"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("image model");
        let baseline = vec![chat.clone(), image.clone()];
        // Re-publishing the chat type replaces only the chat entry.
        let merged = merge_any_models(&baseline, std::slice::from_ref(&chat));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].model_type(), ModelType::Chat);
        assert_eq!(merged[1].model_type(), ModelType::Image);
        // Unknown types are rejected at parse time (callers skip them).
        let unknown = serde_json::from_value::<AnyModel>(serde_json::json!({
            "type": "video",
            "id": "v",
            "name": "V",
            "api": "openai-completions",
            "provider": "p",
            "baseUrl": "https://example.test",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1,
            "maxTokens": 1
        }));
        assert!(unknown.is_err(), "unknown model type must be rejected");
    }

    /// Fetch-based refreshable provider replicating the
    /// `createProvider.fetchModels` refresh body (models.ts:596-616; the
    /// constructor hook itself lands with T15, see module docs).
    struct FetchingTestProvider {
        id: String,
        baseline: Vec<Model>,
        auth: ProviderAuth,
        dynamic: Arc<Mutex<Vec<Model>>>,
        inflight: InflightRefresh,
        fetch_result: Arc<Mutex<Result<Vec<Model>, ModelsError>>>,
        fetch_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FetchingTestProvider {
        fn new(
            id: &str,
            baseline: Vec<Model>,
            fetch_result: Result<Vec<Model>, ModelsError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                id: id.to_owned(),
                baseline,
                auth: ProviderAuth {
                    api_key: Some(Arc::new(StaticKeyAuth)),
                    oauth: None,
                },
                dynamic: Arc::new(Mutex::new(Vec::new())),
                inflight: InflightRefresh::new(),
                fetch_result: Arc::new(Mutex::new(fetch_result)),
                fetch_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })
        }
    }

    impl Provider for FetchingTestProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.id
        }

        fn base_url(&self) -> Option<&str> {
            None
        }

        fn headers(&self) -> Option<&ProviderHeaders> {
            None
        }

        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }

        fn get_models(&self) -> Vec<Model> {
            let dynamic = self.dynamic.lock().unwrap_or_else(|e| e.into_inner());
            merge_models(&self.baseline, &dynamic)
        }

        fn refresh_models(
            &self,
            context: RefreshModelsContext,
        ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
            let id = self.id.clone();
            let dynamic = self.dynamic.clone();
            let fetch_result = self.fetch_result.clone();
            let fetch_calls = self.fetch_calls.clone();
            let inflight = &self.inflight;
            Some(Box::pin(async move {
                inflight
                    .join_or_run(async move {
                        // Phase 1: restore the stored overlay (models.ts:803-816).
                        if let Some(stored) = &context.stored {
                            let dynamic_for_restore = dynamic.clone();
                            let restored: Vec<Model> = stored
                                .models
                                .iter()
                                .filter_map(AnyModel::as_chat)
                                .filter(|m| m.provider == id)
                                .cloned()
                                .collect();
                            let restored_clone = restored.clone();
                            context
                                .publish
                                .publish(ModelsPublication {
                                    persist: None,
                                    update: Some(Box::new(move || {
                                        *dynamic_for_restore
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner()) = restored_clone;
                                    })),
                                })
                                .await?;
                        }
                        if !context.allow_network || context.signal.is_cancelled() {
                            return Ok(());
                        }
                        fetch_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let refreshed = fetch_result
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()?;
                        if context.signal.is_cancelled() {
                            return Ok(());
                        }
                        let dynamic_for_update = dynamic.clone();
                        let refreshed_clone = refreshed.clone();
                        context
                            .publish
                            .publish(ModelsPublication {
                                persist: Some(Some(ModelsStoreEntry {
                                    models: refreshed.into_iter().map(AnyModel::Chat).collect(),
                                    last_modified: None,
                                    checked_at: Some(now_millis()),
                                    etag: None,
                                })),
                                update: Some(Box::new(move || {
                                    *dynamic_for_update.lock().unwrap_or_else(|e| e.into_inner()) =
                                        refreshed_clone;
                                })),
                            })
                            .await?;
                        Ok(())
                    })
                    .await
            }))
        }

        fn stream(
            &self,
            _model: &Model,
            _context: &crate::types::TranscriptContext,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            unreachable!("not used in refresh tests")
        }

        fn stream_simple(
            &self,
            _model: &Model,
            _context: &crate::types::TranscriptContext,
            _options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            unreachable!("not used in refresh tests")
        }
    }

    #[tokio::test]
    async fn test_models_refresh_fetches_and_persists() {
        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(Arc::new(InMemoryModelsStore::new())),
        }));
        let provider = FetchingTestProvider::new(
            "fetchy",
            vec![model_with_id("fetchy", "static")],
            Ok(vec![model_with_id("fetchy", "dynamic")]),
        );
        models.set_provider(provider.clone());

        let result = models.refresh(None).await;
        assert!(!result.aborted);
        assert!(result.errors.is_empty());
        let ids: Vec<String> = models
            .get_models(Some("fetchy"))
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["static".to_owned(), "dynamic".to_owned()]);
        assert_eq!(
            provider
                .fetch_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn test_models_refresh_offline_restores_stored_overlay() {
        // A stored overlay exists; the network fetch fails. The error is
        // recorded and the cached overlay is restored via the offline retry
        // (models.ts:313-322).
        let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
        store
            .write(
                "fetchy",
                ModelsStoreEntry {
                    models: vec![AnyModel::Chat(model_with_id("fetchy", "cached"))],
                    last_modified: None,
                    checked_at: Some(now_millis()),
                    etag: None,
                },
                None,
            )
            .await
            .expect("write");
        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(store),
        }));
        let provider = FetchingTestProvider::new(
            "fetchy",
            vec![model_with_id("fetchy", "static")],
            Err(ModelsError::new(ModelsErrorCode::ModelSource, "boom")),
        );
        models.set_provider(provider.clone());

        let result = models.refresh(None).await;
        assert_eq!(
            result.errors,
            vec![("fetchy".to_owned(), "boom".to_owned())]
        );
        // The overlay survives the failed fetch (offline recovery).
        let ids: Vec<String> = models
            .get_models(Some("fetchy"))
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["static".to_owned(), "cached".to_owned()]);
    }

    #[tokio::test]
    async fn test_models_refresh_skips_unconfigured_and_static_providers() {
        // Unconfigured (no resolvable credential) → skipped without errors.
        struct UnconfiguredAuth;
        #[async_trait::async_trait]
        impl ApiKeyAuth for UnconfiguredAuth {
            fn name(&self) -> &str {
                "Unconfigured"
            }

            async fn resolve(
                &self,
                _ctx: &dyn AuthContext,
                _credential: Option<&crate::auth::ApiKeyCredential>,
            ) -> Result<Option<AuthResult>, ModelsError> {
                Ok(None)
            }
        }
        let models = Models::new(None);
        let unconfigured = Arc::new(UnconfiguredProvider {
            inner: create_provider(CreateProviderOptions {
                id: "ghost".to_owned(),
                name: None,
                base_url: None,
                headers: None,
                auth: ProviderAuth {
                    api_key: Some(Arc::new(UnconfiguredAuth)),
                    oauth: None,
                },
                models: vec![model("ghost", "m")],
                api: ProviderApi::Single(Arc::new(EchoStreams)),
                ..Default::default()
            }),
            inflight: InflightRefresh::new(),
        });
        models.set_provider(unconfigured);
        // Static provider (no refresh_models) — no credential work at all.
        models.set_provider(test_provider(
            "static",
            ProviderApi::Single(Arc::new(EchoStreams)),
        ));

        let result = models.refresh(None).await;
        assert!(result.errors.is_empty());
        assert!(!result.aborted);
    }

    #[tokio::test]
    async fn test_inflight_refresh_dedups_concurrent_runs() {
        let slot = Arc::new(InflightRefresh::new());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let runs_a = runs.clone();
        let runs_b = runs.clone();
        let slot_a = slot.clone();
        let slot_b = slot.clone();
        let join_a = tokio::spawn(async move {
            slot_a
                .join_or_run(async move {
                    runs_a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = start_tx.send(());
                    let _ = done_rx.await;
                    Ok(())
                })
                .await
        });
        let join_b = tokio::spawn(async move {
            slot_b
                .join_or_run(async move {
                    runs_b.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
                .await
        });
        // Wait until the first refresh is in flight, then join from the
        // second caller.
        start_rx.await.expect("started");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 1);
        let _ = done_tx.send(());
        join_a.await.expect("join a").expect("ok");
        join_b.await.expect("join b").expect("ok");
        // A third call after completion starts a fresh run.
        let runs_third = runs.clone();
        slot.join_or_run(async move {
            runs_third.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("fresh run");
        assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    /// Refreshable provider whose auth never resolves (probe/credential
    /// ordering test).
    struct UnconfiguredProvider {
        inner: Arc<dyn Provider>,
        inflight: InflightRefresh,
    }

    impl Provider for UnconfiguredProvider {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn name(&self) -> &str {
            self.inner.name()
        }

        fn base_url(&self) -> Option<&str> {
            self.inner.base_url()
        }

        fn headers(&self) -> Option<&ProviderHeaders> {
            self.inner.headers()
        }

        fn auth(&self) -> &ProviderAuth {
            self.inner.auth()
        }

        fn get_models(&self) -> Vec<Model> {
            self.inner.get_models()
        }

        fn refresh_models(
            &self,
            _context: RefreshModelsContext,
        ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
            let inflight = &self.inflight;
            Some(Box::pin(async move {
                inflight.join_or_run(async { Ok(()) }).await
            }))
        }

        fn stream(
            &self,
            model: &Model,
            context: &crate::types::TranscriptContext,
            options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            self.inner.stream(model, context, options)
        }

        fn stream_simple(
            &self,
            model: &Model,
            context: &crate::types::TranscriptContext,
            options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            self.inner.stream_simple(model, context, options)
        }
    }

    // ------------------------------------------------------------------
    // Two-phase refresh / generation guard / publish API tests
    // (upstream models-runtime.test.ts @ 4181f66)
    // ------------------------------------------------------------------

    /// `envKeyAuth(key)` (models-runtime.test.ts:86-95): resolves from the
    /// stored credential or the fallback ambient key.
    fn env_key_auth(key: Option<&str>) -> Arc<dyn ApiKeyAuth> {
        struct EnvKeyAuth(Option<String>);
        #[async_trait::async_trait]
        impl ApiKeyAuth for EnvKeyAuth {
            fn name(&self) -> &str {
                "Test API key"
            }
            async fn resolve(
                &self,
                _ctx: &dyn AuthContext,
                credential: Option<&ApiKeyCredential>,
            ) -> Result<Option<AuthResult>, ModelsError> {
                let resolved = credential
                    .and_then(|c| c.key.clone())
                    .or_else(|| self.0.clone());
                match resolved {
                    Some(key) => Ok(Some(AuthResult {
                        auth: ModelAuth {
                            api_key: Some(key),
                            headers: None,
                            base_url: None,
                        },
                        env: None,
                        source: Some(
                            if credential.is_some() {
                                "stored"
                            } else {
                                "env"
                            }
                            .to_owned(),
                        ),
                    })),
                    None => Ok(None),
                }
            }
        }
        Arc::new(EnvKeyAuth(key.map(|k| k.to_owned())))
    }

    /// Generic test provider matching the upstream
    /// `testProvider({ refreshModels })` shape: a callback-driven
    /// `refresh_models` with configurable auth and models. The model list is
    /// shared (`Arc<Mutex<...>>`) so callbacks can mutate what `get_models`
    /// returns (upstream `getModels: () => list` semantics).
    struct CallbackProvider {
        id: String,
        models_val: Arc<Mutex<Vec<Model>>>,
        auth_val: ProviderAuth,
        callback: Arc<
            dyn Fn(RefreshModelsContext) -> BoxFuture<'static, Result<(), ModelsError>>
                + Send
                + Sync,
        >,
    }

    impl Provider for CallbackProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn base_url(&self) -> Option<&str> {
            None
        }
        fn headers(&self) -> Option<&ProviderHeaders> {
            None
        }
        fn auth(&self) -> &ProviderAuth {
            &self.auth_val
        }
        fn get_models(&self) -> Vec<Model> {
            self.models_val
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
        fn refresh_models(
            &self,
            context: RefreshModelsContext,
        ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
            Some((self.callback)(context))
        }
        fn stream(
            &self,
            _: &Model,
            _: &crate::types::TranscriptContext,
            _: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            unreachable!("not used in refresh tests")
        }
        fn stream_simple(
            &self,
            _: &Model,
            _: &crate::types::TranscriptContext,
            _: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            unreachable!("not used in refresh tests")
        }
    }

    fn callback_provider(
        id: &str,
        auth: ProviderAuth,
        models: Vec<Model>,
        callback: impl Fn(RefreshModelsContext) -> BoxFuture<'static, Result<(), ModelsError>>
        + Send
        + Sync
        + 'static,
    ) -> Arc<dyn Provider> {
        Arc::new(CallbackProvider {
            id: id.to_owned(),
            models_val: Arc::new(Mutex::new(models)),
            auth_val: auth,
            callback: Arc::new(callback),
        })
    }

    /// OAuth auth whose `refresh` returns a fresh token (test 11).
    struct RefreshingOAuth;
    #[async_trait::async_trait]
    impl crate::auth::OAuthAuth for RefreshingOAuth {
        fn name(&self) -> &str {
            "Test OAuth"
        }
        async fn login(
            &self,
            _: &dyn AuthInteraction,
            _options: Option<&LoginOptions>,
        ) -> Result<crate::auth::OAuthCredential, ModelsError> {
            unreachable!("not used")
        }
        async fn refresh(
            &self,
            _credential: &crate::auth::OAuthCredential,
            _signal: Option<&CancellationToken>,
        ) -> Result<crate::auth::OAuthCredential, ModelsError> {
            Ok(crate::auth::OAuthCredential {
                refresh: "rotated".to_owned(),
                access: "fresh".to_owned(),
                expires: now_millis() + 60_000,
                extra: Map::new(),
            })
        }
        async fn to_auth(
            &self,
            credential: &crate::auth::OAuthCredential,
        ) -> Result<ModelAuth, ModelsError> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                headers: None,
                base_url: None,
            })
        }
    }

    /// `completeSimple` test infrastructure: records `(model, options)` from
    /// each stream call and produces a minimal start+done stream.
    #[derive(Default)]
    #[allow(clippy::type_complexity)]
    struct HeaderRecordingStreams {
        calls: Arc<std::sync::Mutex<Vec<(Model, Option<StreamOptions>)>>>,
    }

    impl ProviderStreams for HeaderRecordingStreams {
        fn stream(
            &self,
            model: &Model,
            _: &crate::types::TranscriptContext,
            options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((model.clone(), options.clone()));
            done_stream(model)
        }
        fn stream_simple(
            &self,
            model: &Model,
            ctx: &crate::types::TranscriptContext,
            options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            Ok(self.stream(model, ctx, options.map(|o| o.stream)))
        }
    }

    fn done_stream(model: &Model) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let partial = crate::types::AssistantMessage {
            role: crate::types::AssistantRole::Assistant,
            content: vec![],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        };
        stream.push(StreamEvent::Start {
            partial: Arc::new(partial.clone()),
        });
        stream.push(StreamEvent::Done {
            reason: DoneReason::Stop,
            message: partial,
        });
        stream.end(None);
        stream
    }

    // --- Test 9: refresh_updates_every_configured_dynamic_provider_and_reports_failures ---

    #[tokio::test]
    async fn refresh_updates_every_configured_dynamic_provider_and_reports_failures() {
        let refreshes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dyn_models: Arc<Mutex<Vec<Model>>> =
            Arc::new(Mutex::new(vec![model_with_id("dyn", "before")]));

        let dyn_models_for_cb = dyn_models.clone();
        let refreshes_clone = refreshes.clone();
        let dyn_provider: Arc<dyn Provider> = Arc::new(CallbackProvider {
            id: "dyn".to_owned(),
            models_val: dyn_models.clone(),
            auth_val: ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            callback: Arc::new(move |context| {
                let dyn_models = dyn_models_for_cb.clone();
                let refreshes = refreshes_clone.clone();
                Box::pin(async move {
                    if !context.allow_network {
                        return Ok(());
                    }
                    refreshes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    context
                        .publish
                        .publish(ModelsPublication {
                            persist: None,
                            update: Some(Box::new(move || {
                                *dyn_models.lock().unwrap_or_else(|e| e.into_inner()) =
                                    vec![model_with_id("dyn", "after")];
                            })),
                        })
                        .await?;
                    Ok(())
                })
            }),
        });

        // Static provider — not refreshable, should not be touched.
        let static_provider = test_provider("static", ProviderApi::Single(Arc::new(EchoStreams)));

        let models = Models::new(None);
        models.set_provider(dyn_provider);
        models.set_provider(static_provider);

        assert!(models.get_model("dyn", "before").is_some());
        let first = models.refresh(None).await;
        assert!(first.errors.is_empty());
        assert_eq!(refreshes.load(std::sync::atomic::Ordering::Relaxed), 1);
        // After refresh, "after" is visible (dynamic overlay replaced "before").
        assert!(models.get_model("dyn", "after").is_some());
        assert!(models.get_model("dyn", "before").is_none());

        // Add a flaky provider that errors when allow_network is true.
        let flaky = callback_provider(
            "flaky",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![],
            move |context| {
                Box::pin(async move {
                    if context.allow_network {
                        return Err(ModelsError::new(
                            ModelsErrorCode::ModelSource,
                            "fetch failed",
                        ));
                    }
                    Ok(())
                })
            },
        );
        models.set_provider(flaky);

        let second = models.refresh(None).await;
        assert_eq!(refreshes.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(
            second.errors,
            vec![("flaky".to_owned(), "fetch failed".to_owned())]
        );
    }

    // --- Test 8: restricts_refresh_work_to_selected_providers ---

    #[tokio::test]
    async fn restricts_refresh_work_to_selected_providers() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let providers: Vec<Arc<dyn Provider>> = ["one", "two"]
            .iter()
            .map(|id| {
                let calls = calls.clone();
                let id_s = id.to_string();
                callback_provider(
                    id,
                    ProviderAuth {
                        api_key: Some(Arc::new(StaticKeyAuth)),
                        oauth: None,
                    },
                    vec![model_with_id(id, "m")],
                    move |context| {
                        let calls = calls.clone();
                        let id_s = id_s.clone();
                        Box::pin(async move {
                            calls
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(format!(
                                    "{}:{}",
                                    id_s,
                                    if context.allow_network {
                                        "network"
                                    } else {
                                        "cache"
                                    }
                                ));
                            Ok(())
                        })
                    },
                )
            })
            .collect();

        let models = Models::new(None);
        for provider in providers {
            models.set_provider(provider);
        }

        let result = models
            .refresh(Some(ModelsRefreshOptions {
                providers: Some(vec!["two".to_owned(), "unknown".to_owned()]),
                ..Default::default()
            }))
            .await;

        assert!(result.errors.is_empty());
        assert_eq!(
            calls.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            vec!["two:cache".to_owned(), "two:network".to_owned()]
        );
    }

    // --- Test 1: restores_cached_models_before_waiting_for_network_auth ---

    #[tokio::test]
    async fn restores_cached_models_before_waiting_for_network_auth() {
        let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
        store
            .write(
                "dynamic",
                ModelsStoreEntry {
                    models: vec![AnyModel::Chat(model_with_id("dynamic", "cached"))],
                    last_modified: None,
                    checked_at: Some(now_millis()),
                    etag: None,
                },
                None,
            )
            .await
            .expect("write");

        // Auth that blocks until `finish_auth` fires.
        let (auth_started_tx, mut auth_started_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (finish_auth_tx, finish_auth_rx) = tokio::sync::mpsc::channel::<()>(1);

        struct BlockingAuth {
            started_tx: tokio::sync::mpsc::Sender<()>,
            finish_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<()>>>,
        }
        #[async_trait::async_trait]
        impl ApiKeyAuth for BlockingAuth {
            fn name(&self) -> &str {
                "Blocked auth"
            }
            async fn resolve(
                &self,
                _ctx: &dyn AuthContext,
                _credential: Option<&ApiKeyCredential>,
            ) -> Result<Option<AuthResult>, ModelsError> {
                let _ = self.started_tx.send(()).await;
                let rx = self.finish_rx.lock().await.take();
                if let Some(mut rx) = rx {
                    let _ = rx.recv().await;
                }
                Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some("key".to_owned()),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some("env".to_owned()),
                }))
            }
        }

        let blocking_auth = Arc::new(BlockingAuth {
            started_tx: auth_started_tx,
            finish_rx: tokio::sync::Mutex::new(Some(finish_auth_rx)),
        });

        let provider = create_provider(CreateProviderOptions {
            id: "dynamic".to_owned(),
            auth: ProviderAuth {
                api_key: Some(blocking_auth),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            fetch_models: Some(Arc::new(|_ctx| {
                Box::pin(async {
                    Err::<Vec<Model>, ModelsError>(ModelsError::new(
                        ModelsErrorCode::ModelSource,
                        "must not fetch",
                    ))
                })
            })),
            ..Default::default()
        });

        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(store),
        }));
        models.set_provider(provider);

        let token = CancellationToken::new();
        let pending = models.refresh(Some(ModelsRefreshOptions {
            signal: Some(token.clone()),
            providers: Some(vec!["dynamic".to_owned()]),
            ..Default::default()
        }));
        tokio::pin!(pending);

        // Drive the refresh future until auth resolution starts (phase 1
        // restore has completed).
        tokio::select! {
            result = &mut pending => panic!("refresh finished before auth started: {result:?}"),
            _ = auth_started_rx.recv() => {}
        }

        // The cached model is visible before auth finishes.
        assert!(models.get_model("dynamic", "cached").is_some());

        // Abort → result is aborted.
        token.cancel();
        let result = (&mut pending).await;
        assert!(result.aborted);
        assert!(result.errors.is_empty());

        // Unblock auth so the background task completes.
        let _ = finish_auth_tx.send(()).await;
    }

    // --- Test 2: lets_providers_choose_persistent_deletion_and_ephemeral_publication_atomically ---

    #[tokio::test]
    async fn lets_providers_choose_persistent_deletion_and_ephemeral_publication_atomically() {
        let entry: Arc<Mutex<Option<ModelsStoreEntry>>> =
            Arc::new(Mutex::new(Some(ModelsStoreEntry {
                models: vec![AnyModel::Chat(model_with_id("dynamic", "stored"))],
                last_modified: None,
                checked_at: Some(now_millis()),
                etag: None,
            })));
        let state = Arc::new(Mutex::new("initial".to_string()));

        struct MockStore {
            entry: Arc<Mutex<Option<ModelsStoreEntry>>>,
        }
        #[async_trait::async_trait]
        impl ModelsStore for MockStore {
            async fn read(
                &self,
                _: &str,
                _: Option<&AuthOperationOptions>,
            ) -> Result<Option<ModelsStoreEntry>, AiError> {
                Ok(self.entry.lock().unwrap().clone())
            }
            async fn write(
                &self,
                _: &str,
                next: ModelsStoreEntry,
                _: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                *self.entry.lock().unwrap() = Some(next);
                Ok(())
            }
            async fn delete(
                &self,
                _: &str,
                _: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                *self.entry.lock().unwrap() = None;
                Ok(())
            }
        }

        let store: Arc<dyn ModelsStore> = Arc::new(MockStore {
            entry: entry.clone(),
        });
        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(store),
        }));

        let state_clone = state.clone();
        let entry_clone = entry.clone();
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![],
            move |context| {
                let state = state_clone.clone();
                let entry = entry_clone.clone();
                Box::pin(async move {
                    // Verify stored is present.
                    assert_eq!(
                        context
                            .stored
                            .as_ref()
                            .and_then(|s| s.models.first())
                            .map(|m| m.id()),
                        Some("stored")
                    );

                    // Publish a deletion.
                    let published = context
                        .publish
                        .publish(ModelsPublication {
                            persist: Some(None),
                            update: Some(Box::new({
                                let entry = entry.clone();
                                let state = state.clone();
                                move || {
                                    assert!(entry.lock().unwrap().is_none());
                                    *state.lock().unwrap() = "deleted".to_string();
                                }
                            })),
                        })
                        .await?;
                    assert!(published);

                    // Publish an ephemeral update (no persist).
                    let published = context
                        .publish
                        .publish(ModelsPublication {
                            persist: None,
                            update: Some(Box::new({
                                let state = state.clone();
                                move || {
                                    *state.lock().unwrap() = "ephemeral".to_string();
                                }
                            })),
                        })
                        .await?;
                    assert!(published);
                    Ok(())
                })
            },
        ));

        let result = models
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            }))
            .await;

        assert!(result.errors.is_empty());
        assert!(entry.lock().unwrap().is_none());
        assert_eq!(*state.lock().unwrap(), "ephemeral");
    }

    // --- Test 3: persists_dynamic_catalogs_and_restores_them_without_network_access ---

    #[tokio::test]
    async fn persists_dynamic_catalogs_and_restores_them_without_network_access() {
        let credentials = Arc::new(InMemoryCredentialStore::new());
        let models_store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
        credentials
            .modify(
                "dynamic",
                Arc::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("key".to_owned()),
                            env: None,
                        })))
                    })
                }),
                None,
            )
            .await
            .expect("seed");

        // Online instance: fetch_models returns a model.
        let online = Models::new(Some(CreateModelsOptions {
            credentials: Some(credentials.clone()),
            auth_context: None,
            models_store: Some(models_store.clone()),
        }));
        online.set_provider(create_provider(CreateProviderOptions {
            id: "dynamic".to_owned(),
            auth: ProviderAuth {
                api_key: Some(env_key_auth(None)),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            fetch_models: Some(Arc::new(|_ctx| {
                Box::pin(async { Ok(vec![model_with_id("dynamic", "fetched")]) })
            })),
            ..Default::default()
        }));

        let result = online.refresh(None).await;
        assert!(result.errors.is_empty());
        assert!(online.get_model("dynamic", "fetched").is_some());

        // Offline instance: fetch_models would throw, but allow_network=false
        // means it never runs — the stored catalog is restored instead.
        let offline = Models::new(Some(CreateModelsOptions {
            credentials: Some(credentials.clone()),
            auth_context: None,
            models_store: Some(models_store.clone()),
        }));
        offline.set_provider(create_provider(CreateProviderOptions {
            id: "dynamic".to_owned(),
            auth: ProviderAuth {
                api_key: Some(env_key_auth(None)),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            fetch_models: Some(Arc::new(|_ctx| {
                Box::pin(async {
                    Err::<Vec<Model>, ModelsError>(ModelsError::new(
                        ModelsErrorCode::ModelSource,
                        "must not fetch",
                    ))
                })
            })),
            ..Default::default()
        }));

        let result = offline
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            }))
            .await;
        assert!(result.errors.is_empty());
        assert!(offline.get_model("dynamic", "fetched").is_some());
    }

    // --- Test 10: passes_effective_api_key_credentials_and_refresh_options ---

    #[tokio::test]
    async fn passes_effective_api_key_credentials_and_refresh_options() {
        let effective_credential: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));
        let force_seen: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let unconfigured_network_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let eff = effective_credential.clone();
        let force_clone = force_seen.clone();
        let configured = callback_provider(
            "configured",
            ProviderAuth {
                api_key: Some(env_key_auth(Some("ambient-key"))),
                oauth: None,
            },
            vec![model_with_id("configured", "m")],
            move |context| {
                let eff = eff.clone();
                let force_clone = force_clone.clone();
                Box::pin(async move {
                    if !context.allow_network {
                        return Ok(());
                    }
                    *eff.lock().unwrap_or_else(|e| e.into_inner()) = context.credential.clone();
                    *force_clone.lock().unwrap_or_else(|e| e.into_inner()) = context.force;
                    Ok(())
                })
            },
        );

        let unconfig_calls = unconfigured_network_calls.clone();
        let unconfigured = callback_provider(
            "unconfigured",
            ProviderAuth {
                api_key: Some(env_key_auth(None)),
                oauth: None,
            },
            vec![model_with_id("unconfigured", "m")],
            move |context| {
                let unconfig_calls = unconfig_calls.clone();
                Box::pin(async move {
                    if context.allow_network {
                        unconfig_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(())
                })
            },
        );

        let models = Models::new(None);
        models.set_provider(configured);
        models.set_provider(unconfigured);

        models
            .refresh(Some(ModelsRefreshOptions {
                force: Some(true),
                ..Default::default()
            }))
            .await;

        assert_eq!(
            effective_credential
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("ambient-key".to_owned()),
                env: None,
            }))
        );
        assert_eq!(
            *force_seen.lock().unwrap_or_else(|e| e.into_inner()),
            Some(true)
        );
        assert_eq!(
            unconfigured_network_calls.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    // --- Test 11: refreshes_expired_oauth_before_refreshing_models ---

    #[tokio::test]
    async fn refreshes_expired_oauth_before_refreshing_models() {
        let credentials = Arc::new(InMemoryCredentialStore::new());
        credentials
            .modify(
                "oauth-dynamic",
                Arc::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(crate::auth::OAuthCredential {
                            access: "expired".to_owned(),
                            refresh: "refresh".to_owned(),
                            expires: 0,
                            extra: Map::new(),
                        })))
                    })
                }),
                None,
            )
            .await
            .expect("seed");

        let model_refresh_credential: Arc<Mutex<Option<Credential>>> = Arc::new(Mutex::new(None));

        let mrc = model_refresh_credential.clone();
        let models = Models::new(Some(CreateModelsOptions {
            credentials: Some(credentials.clone()),
            auth_context: None,
            models_store: None,
        }));
        models.set_provider(callback_provider(
            "oauth-dynamic",
            ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(RefreshingOAuth)),
            },
            vec![model_with_id("oauth-dynamic", "m")],
            move |context| {
                let mrc = mrc.clone();
                Box::pin(async move {
                    if context.allow_network {
                        *mrc.lock().unwrap_or_else(|e| e.into_inner()) = context.credential.clone();
                    }
                    Ok(())
                })
            },
        ));

        let result = models.refresh(None).await;
        assert!(result.errors.is_empty());

        let captured = model_refresh_credential
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("credential captured");
        match captured {
            Credential::OAuth(oauth) => {
                assert_eq!(oauth.access, "fresh");
                assert_eq!(oauth.refresh, "rotated");
            }
            other => panic!("expected OAuth credential, got {other:?}"),
        }

        let stored = credentials
            .read("oauth-dynamic", None)
            .await
            .expect("read")
            .expect("credential");
        match stored {
            Credential::OAuth(oauth) => {
                assert_eq!(oauth.access, "fresh");
                assert_eq!(oauth.refresh, "rotated");
            }
            other => panic!("expected OAuth credential, got {other:?}"),
        }
    }

    // --- Test 4: always_gives_providers_a_concrete_signal ---

    #[tokio::test]
    async fn always_gives_providers_a_concrete_signal() {
        let received: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

        let recv = received.clone();
        let models = Models::new(None);
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![model_with_id("dynamic", "m")],
            move |context| {
                let recv = recv.clone();
                Box::pin(async move {
                    *recv.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.signal.clone());
                    Ok(())
                })
            },
        ));

        let result = models.refresh(None).await;
        assert!(!result.aborted);

        let signal = received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("signal captured");
        assert!(!signal.is_cancelled());
    }

    // --- Test 12: binds_model_store_waits_to_the_provider_refresh_signal ---

    #[tokio::test]
    async fn binds_model_store_waits_to_the_provider_refresh_signal() {
        let storage_signals: Arc<Mutex<Vec<Option<CancellationToken>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let provider_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

        struct SignalRecordingStore {
            signals: Arc<Mutex<Vec<Option<CancellationToken>>>>,
        }
        #[async_trait::async_trait]
        impl ModelsStore for SignalRecordingStore {
            async fn read(
                &self,
                _: &str,
                options: Option<&AuthOperationOptions>,
            ) -> Result<Option<ModelsStoreEntry>, AiError> {
                self.signals
                    .lock()
                    .unwrap()
                    .push(options.and_then(|o| o.signal.clone()));
                Ok(None)
            }
            async fn write(
                &self,
                _: &str,
                _: ModelsStoreEntry,
                options: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                self.signals
                    .lock()
                    .unwrap()
                    .push(options.and_then(|o| o.signal.clone()));
                Ok(())
            }
            async fn delete(
                &self,
                _: &str,
                options: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                self.signals
                    .lock()
                    .unwrap()
                    .push(options.and_then(|o| o.signal.clone()));
                Ok(())
            }
        }

        let store: Arc<dyn ModelsStore> = Arc::new(SignalRecordingStore {
            signals: storage_signals.clone(),
        });
        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(store),
        }));

        let psig = provider_signal.clone();
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(env_key_auth(Some("key"))),
                oauth: None,
            },
            vec![model_with_id("dynamic", "m")],
            move |context| {
                let psig = psig.clone();
                Box::pin(async move {
                    *psig.lock().unwrap_or_else(|e| e.into_inner()) = Some(context.signal.clone());
                    if !context.allow_network {
                        return Ok(());
                    }
                    context
                        .publish
                        .publish(ModelsPublication {
                            persist: Some(Some(ModelsStoreEntry {
                                models: vec![AnyModel::Chat(model_with_id("dynamic", "fresh"))],
                                last_modified: None,
                                checked_at: Some(now_millis()),
                                etag: None,
                            })),
                            update: None,
                        })
                        .await?;
                    Ok(())
                })
            },
        ));

        let result = models
            .refresh(Some(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".to_owned()]),
                ..Default::default()
            }))
            .await;

        assert!(result.errors.is_empty());
        let signals = storage_signals.lock().unwrap().clone();
        assert_eq!(signals.len(), 3, "expected read+read+write");

        let provider_sig = provider_signal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("provider signal captured");

        // All storage signals should be the same token as the provider's
        // signal. Verify by cancelling the provider signal and checking all
        // recorded signals are cancelled (clones share the same inner state).
        provider_sig.cancel();
        for signal in &signals {
            assert!(
                signal.as_ref().is_some_and(|s| s.is_cancelled()),
                "store signal must be the same token as provider signal"
            );
        }
    }

    // --- Test 5: returns_aborted_state_without_reporting_cancellation_as_a_provider_error ---

    #[tokio::test]
    async fn returns_aborted_state_without_reporting_cancellation_as_a_provider_error() {
        let controller = CancellationToken::new();
        let controller_clone = controller.clone();

        let models = Models::new(None);
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![model_with_id("dynamic", "m")],
            move |context| {
                let ctrl = controller_clone.clone();
                Box::pin(async move {
                    ctrl.cancel();
                    if context.signal.is_cancelled() {
                        return Ok(());
                    }
                    Ok(())
                })
            },
        ));

        let result = models
            .refresh(Some(ModelsRefreshOptions {
                signal: Some(controller),
                ..Default::default()
            }))
            .await;
        assert!(result.aborted);
        assert!(result.errors.is_empty());
    }

    // --- Test 6: stops_waiting_on_abort_when_a_provider_ignores_its_signal ---

    #[tokio::test]
    async fn stops_waiting_on_abort_when_a_provider_ignores_its_signal() {
        let controller = CancellationToken::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (resolve_tx, resolve_rx) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));
        let resolve_rx = Arc::new(std::sync::Mutex::new(Some(resolve_rx)));

        let calls_clone = calls.clone();
        let started_tx_clone = started_tx.clone();
        let resolve_rx_clone = resolve_rx.clone();
        let models = Models::new(None);
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![model_with_id("dynamic", "m")],
            move |_context| {
                let calls = calls_clone.clone();
                let started_tx = started_tx_clone.clone();
                let resolve_rx = resolve_rx_clone.clone();
                Box::pin(async move {
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if n != 1 {
                        return Ok(());
                    }
                    if let Some(tx) = started_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    let rx = resolve_rx.lock().unwrap().take();
                    if let Some(rx) = rx {
                        let _ = rx.await;
                    }
                    Ok(())
                })
            },
        ));

        let pending = models.refresh(Some(ModelsRefreshOptions {
            signal: Some(controller.clone()),
            ..Default::default()
        }));
        tokio::pin!(pending);

        // Drive the refresh future until the provider signals started.
        tokio::select! {
            result = &mut pending => panic!("refresh finished before provider started: {result:?}"),
            _ = started_rx => {}
        }

        controller.cancel();

        let result = (&mut pending).await;
        assert!(result.aborted);
        assert!(result.errors.is_empty());

        // Late resolution — the provider's pending resolves, but errors stay empty.
        let _ = resolve_tx.send(());
        tokio::task::yield_now().await;
        assert!(result.errors.is_empty());
    }

    // --- Test 7: rejects_late_publication_from_a_superseded_non_cooperative_provider ---

    #[tokio::test]
    async fn rejects_late_publication_from_a_superseded_non_cooperative_provider() {
        let store: Arc<InMemoryModelsStore> = Arc::new(InMemoryModelsStore::new());
        let state = Arc::new(Mutex::new("initial".to_string()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel::<()>();
        let (finish_first_tx, finish_first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_started_tx = Arc::new(std::sync::Mutex::new(Some(first_started_tx)));
        let finish_first_rx = Arc::new(std::sync::Mutex::new(Some(finish_first_rx)));

        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(store.clone()),
        }));

        let state_clone = state.clone();
        let calls_clone = calls.clone();
        let started_tx_clone = first_started_tx.clone();
        let finish_rx_clone = finish_first_rx.clone();
        models.set_provider(callback_provider(
            "dynamic",
            ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth)),
                oauth: None,
            },
            vec![],
            move |context| {
                let state = state_clone.clone();
                let calls = calls_clone.clone();
                let started_tx = started_tx_clone.clone();
                let finish_rx = finish_rx_clone.clone();
                Box::pin(async move {
                    if !context.allow_network {
                        return Ok(());
                    }
                    let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if n == 1 {
                        if let Some(tx) = started_tx.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        let rx = finish_rx.lock().unwrap().take();
                        if let Some(rx) = rx {
                            let _ = rx.await;
                        }
                    }
                    let value = format!("generation-{n}");
                    context
                        .publish
                        .publish(ModelsPublication {
                            persist: Some(Some(ModelsStoreEntry {
                                models: vec![AnyModel::Chat(model_with_id("dynamic", &value))],
                                last_modified: None,
                                checked_at: Some(now_millis()),
                                etag: None,
                            })),
                            update: Some(Box::new({
                                let state = state.clone();
                                let value = value.clone();
                                move || {
                                    *state.lock().unwrap_or_else(|e| e.into_inner()) = value;
                                }
                            })),
                        })
                        .await?;
                    Ok(())
                })
            },
        ));

        let first = tokio::spawn({
            let models = models.clone();
            async move {
                models
                    .refresh(Some(ModelsRefreshOptions {
                        providers: Some(vec!["dynamic".to_owned()]),
                        ..Default::default()
                    }))
                    .await
            }
        });

        first_started_rx.await.expect("first refresh started");

        let second = models
            .refresh(Some(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".to_owned()]),
                ..Default::default()
            }))
            .await;

        assert!(second.errors.is_empty());
        // Allow the first refresh to settle (it was superseded).
        let _ = first.await;

        // Resolve the first refresh's blocked promise (may fail silently if
        // the future was already dropped).
        let _ = finish_first_tx.send(());

        // Yield to let any background work settle.
        tokio::task::yield_now().await;

        assert_eq!(
            *state.lock().unwrap_or_else(|e| e.into_inner()),
            "generation-2"
        );
        let stored = store.read("dynamic", None).await.expect("read");
        assert_eq!(
            stored
                .as_ref()
                .and_then(|s| s.models.first())
                .map(|m| m.id()),
            Some("generation-2")
        );
    }

    // --- Test 13: passes_caller_signals_to_provider_auth_callbacks_and_login_race ---

    #[tokio::test]
    async fn passes_caller_signals_to_provider_auth_callbacks_and_login_race() {
        // Sub-test 1: credential store modify with a pre-cancelled signal
        // rejects without running the mutation (the mechanism underlying the
        // login write race — auth/types.ts:81-83 @ 4181f66).
        {
            let store = Arc::new(InMemoryCredentialStore::new());
            // Seed an initial credential.
            store
                .modify(
                    "p1",
                    Arc::new(|_| {
                        Box::pin(async {
                            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                key: Some("first".to_owned()),
                                env: None,
                            })))
                        })
                    }),
                    None,
                )
                .await
                .expect("seed");

            // A modify with an already-cancelled signal rejects without running.
            let token = CancellationToken::new();
            token.cancel();
            let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ran_clone = ran.clone();
            let result = store
                .modify(
                    "p1",
                    Arc::new(move |_| {
                        let ran = ran_clone.clone();
                        Box::pin(async move {
                            ran.store(true, std::sync::atomic::Ordering::SeqCst);
                            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                key: Some("second".to_owned()),
                                env: None,
                            })))
                        })
                    }),
                    Some(&AuthOperationOptions::with_signal(token)),
                )
                .await;

            assert!(result.is_err(), "cancelled modify must reject");
            assert!(
                !ran.load(std::sync::atomic::Ordering::SeqCst),
                "mutation must not run"
            );
            let stored = store.read("p1", None).await.expect("read");
            assert_eq!(
                stored,
                Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("first".to_owned()),
                    env: None,
                }))
            );
        }

        // Sub-test 2: a modify with a pre-cancelled signal rejects while a
        // concurrent uncancelled modify succeeds — the credential from the
        // uncancelled modify is persisted, the cancelled one never runs
        // (upstream "cancels queued credential mutations without running them
        // later", models-runtime.test.ts:704-733).
        {
            let store = Arc::new(InMemoryCredentialStore::new());

            // Concurrent uncancelled modify writes "first".
            store
                .modify(
                    "p1",
                    Arc::new(|_| {
                        Box::pin(async {
                            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                key: Some("first".to_owned()),
                                env: None,
                            })))
                        })
                    }),
                    None,
                )
                .await
                .expect("first modify");

            // A second modify with a pre-cancelled signal never runs.
            let token = CancellationToken::new();
            token.cancel();
            let second_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let second_ran_clone = second_ran.clone();
            let opts = AuthOperationOptions::with_signal(token);
            let result = store
                .modify(
                    "p1",
                    Arc::new(move |_| {
                        let ran = second_ran_clone.clone();
                        Box::pin(async move {
                            ran.store(true, std::sync::atomic::Ordering::SeqCst);
                            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                key: Some("second".to_owned()),
                                env: None,
                            })))
                        })
                    }),
                    Some(&opts),
                )
                .await;

            assert!(result.is_err(), "cancelled modify must reject");
            assert!(
                !second_ran.load(std::sync::atomic::Ordering::SeqCst),
                "queued mutation must not run"
            );

            // The first credential survives.
            let stored = store.read("p1", None).await.expect("read");
            assert_eq!(
                stored,
                Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("first".to_owned()),
                    env: None,
                }))
            );
        }
    }

    // --- Test 14: adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once ---

    #[tokio::test]
    async fn adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once() {
        let recording = Arc::new(HeaderRecordingStreams::default());

        let models = Models::new(None);
        models.set_provider(create_provider(CreateProviderOptions {
            id: "p1".to_owned(),
            auth: ProviderAuth {
                api_key: Some(env_key_auth(Some("key"))),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(recording.clone()),
            ..Default::default()
        }));

        let mut model = model_with_id("p1", "model-a");
        model.headers = Some(
            [
                ("x-model".to_owned(), "model".to_owned()),
                ("x-shared".to_owned(), "model".to_owned()),
            ]
            .into_iter()
            .collect(),
        );

        // Provider-level auth: no model headers.
        let provider_auth = models.get_provider_auth("p1", None).await.expect("auth");
        assert!(
            provider_auth
                .as_ref()
                .and_then(|r| r.auth.headers.as_ref())
                .is_none()
        );

        // Model-level auth: model headers merged.
        let model_auth = models.get_auth(&model, None).await.expect("auth");
        let headers = model_auth
            .as_ref()
            .and_then(|r| r.auth.headers.as_ref())
            .expect("headers");
        assert_eq!(headers.get("x-model"), Some(&Some("model".to_owned())));
        assert_eq!(headers.get("x-shared"), Some(&Some("model".to_owned())));

        // Transform runs exactly once on the fully merged headers.
        let transforms = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut options = ModelsSimpleStreamOptions::default();
        options.simple.stream.headers = Some(
            [
                ("x-explicit".to_owned(), Some("explicit".to_owned())),
                ("X-Shared".to_owned(), Some("explicit".to_owned())),
            ]
            .into_iter()
            .collect(),
        );
        let transforms_clone = transforms.clone();
        options.transform_headers = Some(Arc::new(move |headers| {
            let t = transforms_clone.clone();
            Box::pin(async move {
                t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut merged = headers;
                merged.insert("x-transformed".to_owned(), Some("yes".to_owned()));
                merged
            })
        }));

        let _ = models
            .complete_simple(&model, &Context::default(), Some(options))
            .await;

        assert_eq!(
            transforms.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "transform must run exactly once"
        );

        let calls = recording
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(calls.len(), 1);
        let received_headers = calls[0]
            .1
            .as_ref()
            .and_then(|o| o.headers.as_ref())
            .expect("headers");
        assert_eq!(
            received_headers.get("x-model"),
            Some(&Some("model".to_owned()))
        );
        assert_eq!(
            received_headers.get("x-explicit"),
            Some(&Some("explicit".to_owned()))
        );
        assert_eq!(
            received_headers.get("X-Shared"),
            Some(&Some("explicit".to_owned()))
        );
        assert_eq!(
            received_headers.get("x-transformed"),
            Some(&Some("yes".to_owned()))
        );
    }

    // --- Test 15: publish_three_state_golden_and_generation_gates ---

    #[tokio::test]
    async fn publish_three_state_golden_and_generation_gates() {
        fn make_shared(
            store: Arc<dyn ModelsStore>,
            generation: u64,
            refresh_generations: Arc<RwLock<HashMap<String, u64>>>,
        ) -> Arc<PublishShared> {
            Arc::new(PublishShared {
                provider_id: "p".to_owned(),
                generation,
                signal: CancellationToken::new(),
                store,
                chain: Arc::new(tokio::sync::Mutex::new(None)),
                refresh_generations,
            })
        }

        // State 1: persist: None → store unchanged, update runs.
        {
            let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
            store
                .write(
                    "p",
                    ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model_with_id("p", "existing"))],
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("write");
            let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 1u64)])));
            let handle = PublishHandle {
                shared: make_shared(store.clone(), 1, gens),
            };

            let update_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let update_ran_clone = update_ran.clone();
            let applied = handle
                .publish(ModelsPublication {
                    persist: None,
                    update: Some(Box::new(move || {
                        update_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                })
                .await
                .expect("publish ok");

            assert!(applied);
            assert!(update_ran.load(std::sync::atomic::Ordering::SeqCst));
            assert!(
                store.read("p", None).await.expect("read").is_some(),
                "store unchanged"
            );
        }

        // State 2: persist: Some(None) → store entry deleted.
        {
            let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
            store
                .write(
                    "p",
                    ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model_with_id("p", "to-delete"))],
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("write");
            let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 1u64)])));
            let handle = PublishHandle {
                shared: make_shared(store.clone(), 1, gens),
            };

            let applied = handle
                .publish(ModelsPublication {
                    persist: Some(None),
                    update: None,
                })
                .await
                .expect("publish ok");

            assert!(applied);
            assert!(
                store.read("p", None).await.expect("read").is_none(),
                "store deleted"
            );
        }

        // State 3: persist: Some(Some(entry)) → store entry written.
        {
            let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
            let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 1u64)])));
            let handle = PublishHandle {
                shared: make_shared(store.clone(), 1, gens),
            };

            let entry = ModelsStoreEntry {
                models: vec![AnyModel::Chat(model_with_id("p", "written"))],
                ..Default::default()
            };
            let applied = handle
                .publish(ModelsPublication {
                    persist: Some(Some(entry)),
                    update: None,
                })
                .await
                .expect("publish ok");

            assert!(applied);
            let read = store.read("p", None).await.expect("read").expect("entry");
            assert_eq!(read.models[0].id(), "written");
        }

        // Gate 1: supersede generation before publish → returns false, update
        // doesn't run.
        {
            let store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
            let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 2u64)])));
            let handle = PublishHandle {
                shared: make_shared(store.clone(), 1, gens),
            };

            let update_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let update_ran_clone = update_ran.clone();
            let applied = handle
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry::default())),
                    update: Some(Box::new(move || {
                        update_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                })
                .await
                .expect("publish ok");

            assert!(!applied, "superseded publish must return false");
            assert!(
                !update_ran.load(std::sync::atomic::Ordering::SeqCst),
                "update must not run"
            );
        }

        // Gate 2: supersede generation after persist but before update →
        // returns false, update doesn't run (persist already happened).
        {
            struct GenerationBumpingStore {
                inner: Arc<InMemoryModelsStore>,
                gens: Arc<RwLock<HashMap<String, u64>>>,
            }
            #[async_trait::async_trait]
            impl ModelsStore for GenerationBumpingStore {
                async fn read(
                    &self,
                    pid: &str,
                    opts: Option<&AuthOperationOptions>,
                ) -> Result<Option<ModelsStoreEntry>, AiError> {
                    self.inner.read(pid, opts).await
                }
                async fn write(
                    &self,
                    pid: &str,
                    entry: ModelsStoreEntry,
                    opts: Option<&AuthOperationOptions>,
                ) -> Result<(), AiError> {
                    let result = self.inner.write(pid, entry, opts).await;
                    self.gens
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert("p".to_owned(), 2);
                    result
                }
                async fn delete(
                    &self,
                    pid: &str,
                    opts: Option<&AuthOperationOptions>,
                ) -> Result<(), AiError> {
                    self.inner.delete(pid, opts).await
                }
            }

            let inner = Arc::new(InMemoryModelsStore::new());
            let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 1u64)])));
            let bumping: Arc<dyn ModelsStore> = Arc::new(GenerationBumpingStore {
                inner: inner.clone(),
                gens: gens.clone(),
            });
            let handle = PublishHandle {
                shared: make_shared(bumping, 1, gens),
            };

            let update_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let update_ran_clone = update_ran.clone();
            let applied = handle
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model_with_id("p", "gate2"))],
                        ..Default::default()
                    })),
                    update: Some(Box::new(move || {
                        update_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                })
                .await
                .expect("publish ok");

            assert!(!applied, "gate 2 must return false");
            assert!(
                !update_ran.load(std::sync::atomic::Ordering::SeqCst),
                "update must not run"
            );
            // Persist already happened (the write completed before the bump).
            let read = inner.read("p", None).await.expect("read").expect("entry");
            assert_eq!(read.models[0].id(), "gate2");
        }
    }

    // ------------------------------------------------------------------
    // Defect-fix tests: publication chain concurrency + login write race
    // ------------------------------------------------------------------

    /// Verify that two concurrent publishes on the same provider serialize
    /// strictly — the second publish's persist/update runs only after the
    /// first has fully completed (defect-1 regression test).
    #[tokio::test]
    async fn publication_chain_serializes_concurrent_publishes() {
        // We drive two publishes through the same PublishHandle (same chain).
        // The first publish blocks on a controllable gate inside a custom
        // store; the second publish must wait until the first completes.
        let first_started = Arc::new(tokio::sync::Notify::new());
        let first_done = Arc::new(tokio::sync::Notify::new());
        let second_order = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        struct BlockingStore {
            inner: Arc<InMemoryModelsStore>,
            first_started: Arc<tokio::sync::Notify>,
            first_done: Arc<tokio::sync::Notify>,
            write_count: std::sync::atomic::AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ModelsStore for BlockingStore {
            async fn read(
                &self,
                pid: &str,
                opts: Option<&AuthOperationOptions>,
            ) -> Result<Option<ModelsStoreEntry>, AiError> {
                self.inner.read(pid, opts).await
            }
            async fn write(
                &self,
                pid: &str,
                entry: ModelsStoreEntry,
                opts: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                let n = self
                    .write_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    // First write: notify that we started, then block.
                    self.first_started.notify_one();
                    self.first_done.notified().await;
                }
                self.inner.write(pid, entry, opts).await
            }
            async fn delete(
                &self,
                pid: &str,
                opts: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                self.inner.delete(pid, opts).await
            }
        }

        let store: Arc<dyn ModelsStore> = Arc::new(BlockingStore {
            inner: Arc::new(InMemoryModelsStore::new()),
            first_started: first_started.clone(),
            first_done: first_done.clone(),
            write_count: std::sync::atomic::AtomicUsize::new(0),
        });
        let gens = Arc::new(RwLock::new(HashMap::from([("p".to_owned(), 1u64)])));
        let chain = Arc::new(tokio::sync::Mutex::new(None));
        let shared = Arc::new(PublishShared {
            provider_id: "p".to_owned(),
            generation: 1,
            signal: CancellationToken::new(),
            store: store.clone(),
            chain,
            refresh_generations: gens,
        });
        let handle = PublishHandle { shared };

        // Launch the first publish (blocks in store.write).
        let second_order_clone = second_order.clone();
        let handle1 = handle.clone();
        let first = tokio::spawn(async move {
            handle1
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model_with_id("p", "first"))],
                        ..Default::default()
                    })),
                    update: Some(Box::new(move || {
                        second_order_clone.store(1, std::sync::atomic::Ordering::SeqCst);
                    })),
                })
                .await
                .expect("first publish")
        });

        // Wait for the first publish to enter its store.write.
        first_started.notified().await;

        // Launch the second publish — it must NOT run until the first completes.
        let second_order_clone2 = second_order.clone();
        let handle2 = handle.clone();
        let second = tokio::spawn(async move {
            handle2
                .publish(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(model_with_id("p", "second"))],
                        ..Default::default()
                    })),
                    update: Some(Box::new(move || {
                        second_order_clone2.store(2, std::sync::atomic::Ordering::SeqCst);
                    })),
                })
                .await
                .expect("second publish")
        });

        // Give the second publish a chance to (incorrectly) proceed.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The second publish must not have run yet (order still 0 — the
        // first publish's update hasn't run either because store.write is
        // still blocking).
        assert_eq!(
            second_order.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "neither publish's update should have run while first is blocking"
        );

        // Release the first publish.
        first_done.notify_one();
        first.await.expect("first join");

        // Now the second publish can proceed.
        second.await.expect("second join");

        // The first publish's update must have run before the second's
        // (order went 0→1→2, not 0→2→1 or 0→2).
        assert_eq!(
            second_order.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "second publish's update must run after first completes"
        );

        // Store should have the second entry (last writer wins).
        let entry = store.read("p", None).await.expect("read").expect("entry");
        assert_eq!(entry.models[0].id(), "second");
    }

    /// Login credential write race: when the modify is queued behind a
    // blocking predecessor and the caller cancels while queued (mutation fn
    // has not started), the credential must NOT be written.
    #[tokio::test]
    async fn login_cancels_queued_write_without_running_mutation() {
        use crate::auth::{ApiKeyAuth, AuthResult, ModelAuth};

        // Credential store — login goes through this store. We use a
        // DelayedStore wrapper instead (below).
        let store = Arc::new(crate::auth::InMemoryCredentialStore::new());

        // Block the first modify so the login's modify queues behind it.
        let blocker_done = Arc::new(tokio::sync::Notify::new());
        let blocker_started = Arc::new(tokio::sync::Notify::new());

        // Seed a blocking modify first.
        let store_clone = store.clone();
        let blocker_started_clone = blocker_started.clone();
        let blocker_done_clone = blocker_done.clone();
        let blocker = tokio::spawn(async move {
            store_clone
                .modify(
                    "test",
                    Arc::new(move |_| {
                        blocker_started_clone.notify_one();
                        let done = blocker_done_clone.clone();
                        Box::pin(async move {
                            done.notified().await;
                            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                key: Some("first".to_owned()),
                                env: None,
                            })))
                        })
                    }),
                    None,
                )
                .await
                .expect("blocker modify");
        });

        // Wait for the blocker to start holding the modify lock.
        blocker_started.notified().await;

        // Provider with login support.
        struct LoginAuth;
        #[async_trait::async_trait]
        impl ApiKeyAuth for LoginAuth {
            fn name(&self) -> &str {
                "Test"
            }
            fn supports_login(&self) -> bool {
                true
            }
            async fn login(
                &self,
                _interaction: &dyn AuthInteraction,
            ) -> Result<ApiKeyCredential, ModelsError> {
                Ok(ApiKeyCredential {
                    key: Some("logged-in".to_owned()),
                    env: None,
                })
            }
            async fn resolve(
                &self,
                _ctx: &dyn AuthContext,
                _credential: Option<&ApiKeyCredential>,
            ) -> Result<Option<AuthResult>, ModelsError> {
                Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some("resolved".to_owned()),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: None,
                }))
            }
        }

        // Interaction with a caller signal.
        struct SignalInteraction {
            token: CancellationToken,
        }
        impl AuthInteraction for SignalInteraction {
            fn signal(&self) -> Option<CancellationToken> {
                Some(self.token.clone())
            }
            fn prompt<'a>(
                &'a self,
                _prompt: AuthPrompt,
            ) -> crate::auth::types::BoxFutureSend<'a, Result<String, ModelsError>> {
                Box::pin(async { Ok("unused".to_owned()) })
            }
            fn notify(&self, _event: AuthEvent) {}
        }

        let models = Models::new(Some(CreateModelsOptions {
            credentials: Some(store.clone()),
            auth_context: None,
            models_store: None,
        }));
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(LoginAuth)),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));

        // Login with a cancellation token.
        let token = CancellationToken::new();
        let interaction = SignalInteraction {
            token: token.clone(),
        };

        // Start login — it will queue behind the blocker.
        let models_clone = models.clone();
        let login_task = tokio::spawn(async move {
            models_clone
                .login("test", AuthType::ApiKey, &interaction, None)
                .await
        });

        // Give the login a moment to queue its modify.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Cancel while the modify is still queued (mutation fn hasn't started).
        token.cancel();

        // Login should reject with an error.
        let result = login_task.await.expect("join");
        assert!(
            result.is_err(),
            "login must reject when cancelled while queued"
        );

        // Release the blocker.
        blocker_done.notify_one();
        blocker.await.expect("blocker join");

        // The credential must NOT have been written — only the blocker's
        // "first" key should be in the store.
        let stored = store.read("test", None).await.expect("read");
        match stored {
            Some(Credential::ApiKey(cred)) => {
                assert_eq!(
                    cred.key,
                    Some("first".to_owned()),
                    "login credential must NOT be written; only blocker's"
                );
            }
            other => panic!("expected blocker's credential, got {other:?}"),
        }
    }

    /// Login credential write race (started path): when the mutation fn has
    /// already started (flag set) and the caller cancels, the login must
    /// still await the modify to completion and return the credential.
    /// The store must contain the credential (models.ts:594 @ 4181f66).
    #[tokio::test]
    async fn login_completes_write_when_mutation_started_before_cancel() {
        use crate::auth::{
            ApiKeyAuth, AuthOperationOptions, AuthResult, CredentialInfo, CredentialStore,
            ModelAuth,
        };

        /// Store wrapper that delays modify completion: the inner modify runs
        /// and completes (setting mutation_started), then we sleep so the
        /// caller can cancel while the write is "in flight" but already
        /// committed.
        struct DelayedStore {
            inner: crate::auth::InMemoryCredentialStore,
            delay: std::time::Duration,
        }

        #[async_trait::async_trait]
        impl CredentialStore for DelayedStore {
            async fn read(
                &self,
                provider_id: &str,
                options: Option<&AuthOperationOptions>,
            ) -> Result<Option<Credential>, ModelsError> {
                self.inner.read(provider_id, options).await
            }
            async fn list(
                &self,
                options: Option<&AuthOperationOptions>,
            ) -> Result<Vec<CredentialInfo>, ModelsError> {
                self.inner.list(options).await
            }
            async fn modify(
                &self,
                provider_id: &str,
                f: crate::auth::types::ModifyFn,
                options: Option<&AuthOperationOptions>,
            ) -> Result<Option<Credential>, ModelsError> {
                let result = self.inner.modify(provider_id, f, options).await?;
                tokio::time::sleep(self.delay).await;
                Ok(result)
            }
            async fn delete(
                &self,
                provider_id: &str,
                options: Option<&AuthOperationOptions>,
            ) -> Result<(), ModelsError> {
                self.inner.delete(provider_id, options).await
            }
        }

        let delayed_store: Arc<dyn CredentialStore> = Arc::new(DelayedStore {
            inner: crate::auth::InMemoryCredentialStore::new(),
            delay: std::time::Duration::from_millis(100),
        });

        struct LoginAuth;
        #[async_trait::async_trait]
        impl ApiKeyAuth for LoginAuth {
            fn name(&self) -> &str {
                "Test"
            }
            fn supports_login(&self) -> bool {
                true
            }
            async fn login(
                &self,
                _interaction: &dyn AuthInteraction,
            ) -> Result<ApiKeyCredential, ModelsError> {
                Ok(ApiKeyCredential {
                    key: Some("logged-in".to_owned()),
                    env: None,
                })
            }
            async fn resolve(
                &self,
                _ctx: &dyn AuthContext,
                _credential: Option<&ApiKeyCredential>,
            ) -> Result<Option<AuthResult>, ModelsError> {
                Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some("resolved".to_owned()),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: None,
                }))
            }
        }

        struct SignalInteraction {
            token: CancellationToken,
        }
        impl AuthInteraction for SignalInteraction {
            fn signal(&self) -> Option<CancellationToken> {
                Some(self.token.clone())
            }
            fn prompt<'a>(
                &'a self,
                _prompt: AuthPrompt,
            ) -> crate::auth::types::BoxFutureSend<'a, Result<String, ModelsError>> {
                Box::pin(async { Ok("unused".to_owned()) })
            }
            fn notify(&self, _event: AuthEvent) {}
        }

        let models = Models::new(Some(CreateModelsOptions {
            credentials: Some(delayed_store.clone()),
            auth_context: None,
            models_store: None,
        }));
        models.set_provider(create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(LoginAuth)),
                oauth: None,
            },
            models: vec![],
            api: ProviderApi::Single(Arc::new(EchoStreams)),
            ..Default::default()
        }));

        let token = CancellationToken::new();
        let interaction = SignalInteraction {
            token: token.clone(),
        };

        let models_clone = models.clone();
        let login_task = tokio::spawn(async move {
            models_clone
                .login("test", AuthType::ApiKey, &interaction, None)
                .await
        });

        // Wait 50ms — the mutation fn has started (flag set) but the
        // DelayedStore's sleep hasn't finished. Cancel mid-flight.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        token.cancel();

        // Login must succeed — mutation started, so the write completes.
        let credential = login_task.await.expect("join").expect("login ok");
        match credential {
            Credential::ApiKey(cred) => {
                assert_eq!(cred.key, Some("logged-in".to_owned()));
            }
            other => panic!("expected api_key credential, got {other:?}"),
        }

        // Store must contain the credential.
        let stored = delayed_store.read("test", None).await.expect("read");
        match stored {
            Some(Credential::ApiKey(cred)) => {
                assert_eq!(cred.key, Some("logged-in".to_owned()));
            }
            other => panic!("expected stored api_key credential, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // credential-resolved baseUrl override (R3.4.8, commit e741cb05c,
    // model-runtime.ts:600, apply_auth in models.rs:1451-1457)
    // ------------------------------------------------------------------

    /// Auth that returns a `base_url` override in its resolved result —
    /// simulating a credential-resolved endpoint (e.g. subscription proxy).
    struct BaseUrlOverrideAuth;

    #[async_trait::async_trait]
    impl ApiKeyAuth for BaseUrlOverrideAuth {
        fn name(&self) -> &str {
            "Override API key"
        }

        async fn resolve(
            &self,
            _ctx: &dyn AuthContext,
            _credential: Option<&ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("sk-override".to_owned()),
                    headers: None,
                    base_url: Some("https://credential-resolved.example.com/v1".to_owned()),
                },
                env: None,
                source: Some("OVERRIDE".to_owned()),
            }))
        }
    }

    /// A streams impl that records the model's base_url at dispatch time so
    /// the test can assert the override was applied.
    struct RecordingBaseUrlStreams {
        captured_base_url: Arc<Mutex<Option<String>>>,
    }

    impl ProviderStreams for RecordingBaseUrlStreams {
        fn stream(
            &self,
            model: &Model,
            _context: &crate::types::TranscriptContext,
            options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            *self.captured_base_url.lock().unwrap() = Some(model.base_url.clone());
            let stream = AssistantMessageEventStream::new();
            let mut partial = crate::types::AssistantMessage {
                role: crate::types::AssistantRole::Assistant,
                content: vec![],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: options.and_then(|o| o.request.api_key),
                timestamp: 0,
                deferred: None,
                end_turn: None,
                raw_stop_reason: None,
                duration_ms: None,
            };
            partial.stop_reason = StopReason::Stop;
            stream.push(StreamEvent::Done {
                reason: DoneReason::Stop,
                message: partial,
            });
            stream.end(None);
            stream
        }

        fn stream_simple(
            &self,
            model: &Model,
            context: &crate::types::TranscriptContext,
            _options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            Ok(self.stream(model, context, None))
        }
    }

    /// `apply_auth` applies a credential-resolved `base_url` to the request
    /// model — both the main stream and `streamSimple` paths
    /// (R3.4.8, model-runtime.ts:600 @ e741cb05c).
    #[tokio::test]
    async fn test_apply_auth_overrides_base_url_from_credential_resolution() {
        let captured = Arc::new(Mutex::new(None));
        let streams = Arc::new(RecordingBaseUrlStreams {
            captured_base_url: captured.clone(),
        });
        let provider = create_provider(CreateProviderOptions {
            id: "override-test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(BaseUrlOverrideAuth)),
                oauth: None,
            },
            models: vec![model("override-test", ApiKind::ANTHROPIC_MESSAGES)],
            api: ProviderApi::Single(streams),
            ..Default::default()
        });
        let models = Models::new(None);
        models.set_provider(provider);
        let model = models.get_model("override-test", "m").expect("model");

        // The model's static base_url is the fixture default.
        assert_eq!(model.base_url, "https://example.com");

        // Main stream path: credential-resolved base_url overrides.
        let _ = models
            .complete(&model, &Context::default(), None)
            .await
            .expect("result");
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("https://credential-resolved.example.com/v1"),
            "main stream path must use the credential-resolved base_url"
        );

        // streamSimple path: same override applies.
        captured.lock().unwrap().take();
        let _ = models
            .complete_simple(&model, &Context::default(), None)
            .await;
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("https://credential-resolved.example.com/v1"),
            "streamSimple path must use the credential-resolved base_url"
        );
    }

    /// T21 fix: a models-store read failure in `run_provider_refresh_phase`
    /// propagates (models.ts:375 has no catch) and lands in the refresh
    /// `errors` map (models.ts:422-429).
    #[tokio::test]
    async fn refresh_reports_models_store_read_failures() {
        struct FailingReadStore;
        #[async_trait::async_trait]
        impl ModelsStore for FailingReadStore {
            async fn read(
                &self,
                _: &str,
                _: Option<&AuthOperationOptions>,
            ) -> Result<Option<ModelsStoreEntry>, AiError> {
                Err(AiError::CredentialStore("read boom".to_owned()))
            }
            async fn write(
                &self,
                _: &str,
                _: ModelsStoreEntry,
                _: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                Ok(())
            }
            async fn delete(
                &self,
                _: &str,
                _: Option<&AuthOperationOptions>,
            ) -> Result<(), AiError> {
                Ok(())
            }
        }

        let provider = callback_provider("dyn", ProviderAuth::default(), vec![], |_context| {
            Box::pin(async { Ok(()) })
        });
        let models = Models::new(Some(CreateModelsOptions {
            credentials: None,
            auth_context: None,
            models_store: Some(Arc::new(FailingReadStore)),
        }));
        models.set_provider(provider);
        let result = models
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            }))
            .await;
        assert!(!result.aborted);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, "dyn");
        assert!(result.errors[0].1.contains("read boom"));
    }

    /// T21 fix: when the caller cancels mid-refresh, the outer `select!`
    /// drops the per-provider future; the `ProviderRefreshGuard` must still
    /// abort the signal watcher and remove the controller from
    /// `refresh_controllers` (models.ts:431-435 `finally`).
    #[tokio::test]
    async fn refresh_removes_controller_when_caller_cancels_mid_refresh() {
        let started = Arc::new(tokio::sync::Notify::new());
        let started_in_callback = started.clone();
        let provider = callback_provider("dyn", ProviderAuth::default(), vec![], move |_context| {
            let started = started_in_callback.clone();
            Box::pin(async move {
                started.notify_one();
                // Hang until the caller's cancellation drops this future.
                std::future::pending::<()>().await;
                Ok(())
            })
        });
        let models = Models::new(None);
        models.set_provider(provider);
        let token = CancellationToken::new();
        let models_clone = models.clone();
        let token_clone = token.clone();
        let refresh_task = tokio::spawn(async move {
            models_clone
                .refresh(Some(ModelsRefreshOptions {
                    signal: Some(token_clone),
                    ..Default::default()
                }))
                .await
        });
        started.notified().await;
        token.cancel();
        let result = refresh_task.await.expect("join");
        assert!(result.aborted);
        assert!(
            models
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
    }

    // ====================================================================
    // V16-06 FR-G: type accessors + chat guard
    // ====================================================================

    /// All-type provider for accessor tests: chat + image + classifier.
    struct AllTypesProvider {
        models: Vec<AnyModel>,
        auth: ProviderAuth,
    }

    impl Provider for AllTypesProvider {
        fn id(&self) -> &str {
            "all-types"
        }

        fn name(&self) -> &str {
            "All Types"
        }

        fn base_url(&self) -> Option<&str> {
            None
        }

        fn headers(&self) -> Option<&ProviderHeaders> {
            None
        }

        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }

        fn get_models(&self) -> Vec<Model> {
            self.models
                .iter()
                .filter_map(|model| match model {
                    AnyModel::Chat(chat) => Some(chat.clone()),
                    _ => None,
                })
                .collect()
        }

        fn get_all_models(&self) -> Vec<AnyModel> {
            self.models.clone()
        }

        fn stream(
            &self,
            _model: &Model,
            _context: &crate::types::TranscriptContext,
            _options: Option<StreamOptions>,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }

        fn stream_simple(
            &self,
            _model: &Model,
            _context: &crate::types::TranscriptContext,
            _options: Option<SimpleStreamOptions>,
        ) -> Result<AssistantMessageEventStream, String> {
            Ok(AssistantMessageEventStream::new())
        }
    }

    fn all_types_provider(auth: ProviderAuth) -> Arc<dyn Provider> {
        let chat = model("all-types", ApiKind::ANTHROPIC_MESSAGES);
        let image: AnyModel = serde_json::from_value(json!({
            "type": "image",
            "id": "img-1",
            "name": "Image One",
            "api": "openrouter-images",
            "provider": "all-types",
            "baseUrl": "https://example.test",
            "input": ["text"],
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
            "output": ["image"]
        }))
        .expect("image model");
        let classifier: AnyModel = serde_json::from_value(json!({
            "type": "classifier",
            "id": "jev-1",
            "name": "Jev One",
            "api": "openai-completions",
            "provider": "all-types",
            "baseUrl": "https://example.test",
            "input": ["text"],
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
            "contextWindow": 8192
        }))
        .expect("classifier model");
        Arc::new(AllTypesProvider {
            models: vec![AnyModel::Chat(chat), image, classifier],
            auth,
        })
    }

    #[test]
    fn get_models_of_type_filters_by_type_and_keeps_chat_only_reads() {
        let models = Models::new(None);
        models.set_provider(all_types_provider(ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        }));

        // `get_models` stays chat-only (red line).
        let chat: Vec<String> = models
            .get_models(None)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(chat, vec!["m".to_owned()]);

        let all: Vec<AnyModel> = models.get_all_models(None);
        assert_eq!(all.len(), 3);

        let images = models.get_models_of_type(ModelType::Image, None);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].id(), "img-1");
        let classifiers = models.get_models_of_type(ModelType::Classifier, None);
        assert_eq!(classifiers.len(), 1);
        assert_eq!(classifiers[0].id(), "jev-1");
        assert_eq!(
            models
                .get_models_of_type(ModelType::Chat, None)
                .into_iter()
                .map(|model| model.id().to_owned())
                .collect::<Vec<_>>(),
            vec!["m".to_owned()]
        );

        assert_eq!(
            models
                .get_model_of_type(ModelType::Image, "all-types", "img-1")
                .map(|model| model.id().to_owned()),
            Some("img-1".to_owned())
        );
        assert!(
            models
                .get_model_of_type(ModelType::Image, "all-types", "m")
                .is_none()
        );
    }

    #[tokio::test]
    async fn availability_accessors_gate_on_auth_and_filter_chat_models() {
        let models = Models::new(None);
        models.set_provider(all_types_provider(ProviderAuth {
            api_key: Some(Arc::new(StaticKeyAuth)),
            oauth: None,
        }));
        let available = models.get_all_available(None).await.expect("available");
        assert_eq!(available.len(), 3);
        let images = models
            .get_available_of_type(ModelType::Image, None)
            .await
            .expect("available images");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].id(), "img-1");

        // No configured auth → no available models of any type.
        let models = Models::new(None);
        models.set_provider(all_types_provider(ProviderAuth {
            api_key: None,
            oauth: None,
        }));
        assert!(
            models
                .get_all_available(None)
                .await
                .expect("empty")
                .is_empty()
        );
        assert!(
            models
                .get_available_of_type(ModelType::Image, None)
                .await
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn assert_chat_model_rejects_non_chat_models() {
        let chat = AnyModel::Chat(model("all-types", ApiKind::ANTHROPIC_MESSAGES));
        let chat = assert_chat_model(&chat).expect("chat accepted");
        assert_eq!(chat.id, "m");

        let image: AnyModel = serde_json::from_value(json!({
            "type": "image",
            "id": "img-1",
            "name": "Image One",
            "api": "openrouter-images",
            "provider": "all-types",
            "baseUrl": "https://example.test",
            "input": ["text"],
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
            "output": ["image"]
        }))
        .expect("image model");
        let error = assert_chat_model(&image).expect_err("image rejected");
        assert_eq!(error.code, ModelsErrorCode::Provider);
        assert!(error.message.contains("is not a chat model"));
    }
}
