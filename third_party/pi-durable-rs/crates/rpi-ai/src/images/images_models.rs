//! Port of `packages/ai/src/images-models.ts` @ pi 0.82.1 (2efa728).
//!
//! The image-generation counterpart of `models.rs`: [`ProviderImages`]
//! (adapter interface), [`ImagesProvider`] (provider trait),
//! [`ImagesModels`] (runtime collection with auth application and
//! never-reject generation), `create_images_provider` (with in-flight
//! refresh dedupe) and `create_images_models`.
//!
//! Intentional differences:
//! - `getModels()`'s try/catch around ill-behaved providers has no Rust
//!   counterpart: the trait method is total (a panicking implementation
//!   would unwind; the upstream "best-effort yields no models" catch is
//!   documented in D-036).
//! - `refresh` wraps any provider refresh failure in `ModelsError`
//!   ("model_source"); upstream's `instanceof ModelsError` pass-through has
//!   no reachable case here (no image provider throws a typed `ModelsError`
//!   from its fetch; the fetch error channel carries a message string).
//! - `ImagesModels::generate_images` never rejects: provider/auth failures
//!   are returned as an `AssistantImages` with `stopReason: "error"` and the
//!   error message.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use futures::future::{FutureExt, Shared, join_all};

use crate::auth::{
    AuthContext, AuthResolutionOverrides, AuthResult, CredentialStore, DefaultAuthContext,
    InMemoryCredentialStore, ModelsError, ModelsErrorCode, ProviderAuth, resolve_provider_auth,
};
use crate::models::CreateModelsOptions;
use crate::models_json::OrderedMap;
use crate::types::{
    AssistantImages, ImagesContext, ImagesModel, ImagesOptions, ImagesStopReason, ProviderEnv,
};
use crate::utils::headers::merge_headers;

/// Unix timestamp in milliseconds (`Date.now()` upstream).
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `ProviderImages` — the uniform contract of an image-generation API
/// implementation module (upstream: every image api module exports exactly
/// `generateImages`; the interface itself is the module value).
pub trait ProviderImages: Send + Sync {
    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>>;
}

/// Dynamic refresh function: fetch the current model list; `Err` carries the
/// raw error message (wrapped by [`ImagesModels::refresh`]).
pub type ImagesRefreshFn = dyn Fn() -> Pin<Box<dyn Future<Output = Result<Vec<ImagesModel>, String>> + Send + 'static>>
    + Send
    + Sync;

/// Boxed refresh future returned by [`ImagesProvider::refresh_models`].
pub type ImagesRefreshFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;

/// `ImagesProvider` — an image-generation provider: id/name metadata, auth,
/// model listing, and generation behavior (the image-side counterpart of
/// `crate::models::Provider`).
pub trait ImagesProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    /// Required: at least one of `apiKey`/`oauth`. Same semantics as chat
    /// providers; `ImagesModels::get_auth` returns `None` when the provider
    /// is unconfigured.
    fn auth(&self) -> &ProviderAuth;

    /// Current known models, sync. Static providers return their catalog;
    /// dynamic providers the list as of the last `refresh_models()` (empty
    /// before the first).
    fn get_models(&self) -> Vec<ImagesModel>;

    /// Dynamic providers only: fetch and update the model list; `Err` on
    /// network failure (the list stays at its last-known state and a later
    /// call retries). Static providers return `None`.
    fn refresh_models(&self) -> Option<ImagesRefreshFuture> {
        None
    }

    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>>;
}

/// `CreateImagesProviderOptions`.
pub struct CreateImagesProviderOptions {
    pub id: String,
    /// Display name. Default: `id`.
    pub name: Option<String>,
    /// Required — every provider has auth semantics, even ambient/keyless ones.
    pub auth: ProviderAuth,
    /// Initial model list (empty for purely dynamic providers).
    pub models: Vec<ImagesModel>,
    /// Dynamic providers: fetch the current list. Stored on success;
    /// concurrent calls share one in-flight fetch. May fail: the stored list
    /// then stays at its last-known state and a later call retries.
    pub refresh_models: Option<Arc<ImagesRefreshFn>>,
    pub api: Arc<dyn ProviderImages>,
}

type SharedRefresh = Shared<ImagesRefreshFuture>;

struct CreatedImagesProvider {
    id: String,
    name: String,
    auth: ProviderAuth,
    models: Arc<RwLock<Vec<ImagesModel>>>,
    inflight_refresh: Arc<Mutex<Option<SharedRefresh>>>,
    refresh_models_fn: Option<Arc<ImagesRefreshFn>>,
    api: Arc<dyn ProviderImages>,
}

impl CreatedImagesProvider {
    /// `createImagesProvider`'s closure-based in-flight dedupe
    /// (`inflightRefresh ??= ...`): concurrent callers share one shared
    /// future; the slot is cleared on completion (success or failure), so a
    /// later call retries.
    fn refresh_models(&self) -> Option<ImagesRefreshFuture> {
        let refresh = self.refresh_models_fn.clone()?;
        let mut guard = self
            .inflight_refresh
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let shared = match guard.as_ref() {
            Some(shared) => shared.clone(),
            None => {
                let models = Arc::clone(&self.models);
                let future: Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>> =
                    Box::pin(async move {
                        let next = refresh().await?;
                        *models.write().unwrap_or_else(|e| e.into_inner()) = next;
                        Ok(())
                    });
                let shared = future.shared();
                *guard = Some(shared.clone());
                shared
            }
        };
        drop(guard);
        let inflight = Arc::clone(&self.inflight_refresh);
        Some(Box::pin(async move {
            let result = shared.await;
            inflight.lock().unwrap_or_else(|e| e.into_inner()).take();
            result
        }))
    }
}

impl ImagesProvider for CreatedImagesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<ImagesModel> {
        self.models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn refresh_models(&self) -> Option<ImagesRefreshFuture> {
        self.refresh_models()
    }

    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
        self.api.generate_images(model, context, options)
    }
}

/// `createImagesProvider` — builds a provider from parts.
pub fn create_images_provider(input: CreateImagesProviderOptions) -> Arc<dyn ImagesProvider> {
    Arc::new(CreatedImagesProvider {
        name: input.name.unwrap_or_else(|| input.id.clone()),
        id: input.id,
        auth: input.auth,
        models: Arc::new(RwLock::new(input.models)),
        inflight_refresh: Arc::new(Mutex::new(None)),
        refresh_models_fn: input.refresh_models,
        api: input.api,
    })
}

/// `ImagesModels` — runtime collection of image-generation providers plus
/// auth application and generation convenience: the image-side counterpart of
/// [`Models`].
#[derive(Clone)]
pub struct ImagesModels {
    /// Insertion-ordered like the upstream JS `Map`: `get_providers()` /
    /// `get_models()` order is observable.
    providers: Arc<RwLock<OrderedMap<Arc<dyn ImagesProvider>>>>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
}

impl Default for ImagesModels {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ImagesModels {
    /// `createImagesModels`.
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
        }
    }

    /// `setProvider` — upsert/replace by `provider.id`. Provider ids are unique.
    pub fn set_provider(&self, provider: Arc<dyn ImagesProvider>) {
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider.id().to_owned(), provider);
    }

    pub fn delete_provider(&self, id: &str) {
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn clear_providers(&self) {
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn ImagesProvider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn ImagesProvider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// `getModels` — sync read of last-known models from one provider or all.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<ImagesModel> {
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

    /// `getModel` — sync runtime model lookup.
    pub fn get_model(&self, provider: &str, id: &str) -> Option<ImagesModel> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    /// `refresh` — ask dynamic providers to re-fetch their model lists. With
    /// a provider id, rejects with `ModelsError` ("model_source") on that
    /// provider's fetch failure; without one, refreshes all providers
    /// concurrently best-effort (never rejects). Static providers are no-ops.
    pub async fn refresh(&self, provider: Option<&str>) -> Result<(), ModelsError> {
        match provider {
            Some(id) => {
                let Some(entry) = self.get_provider(id) else {
                    return Ok(());
                };
                let Some(future) = entry.refresh_models() else {
                    return Ok(());
                };
                match future.await {
                    Ok(()) => Ok(()),
                    Err(message) => Err(ModelsError::with_cause(
                        ModelsErrorCode::ModelSource,
                        format!("Model refresh failed for {id}"),
                        &message,
                    )),
                }
            }
            None => {
                let providers = self.get_providers();
                let futures: Vec<_> = providers
                    .iter()
                    .filter_map(|provider| provider.refresh_models())
                    .collect();
                join_all(futures).await;
                Ok(())
            }
        }
    }

    /// `getAuth(providerId)` — resolve request auth by provider id. `None`
    /// when unknown/unconfigured; rejects with `ModelsError` on real
    /// failures.
    pub async fn get_auth_for_provider(
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

    /// `getAuth(model)` — same, keyed by the model's provider.
    pub async fn get_auth(
        &self,
        model: &ImagesModel,
        overrides: Option<&AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        self.get_auth_for_provider(&model.provider, overrides).await
    }

    /// `generateImages` — generate through the owning provider with auth
    /// resolved and merged (explicit options win per field). **Never
    /// rejects**: failures are returned as an `AssistantImages` with
    /// `stopReason: "error"` and `errorMessage`.
    pub async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        match self.generate_images_inner(model, context, options).await {
            Ok(images) => images,
            Err(error) => AssistantImages {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                output: Vec::new(),
                response_id: None,
                usage: None,
                stop_reason: ImagesStopReason::Error,
                error_message: Some(error.message),
                timestamp: now_ms(),
            },
        }
    }

    async fn generate_images_inner(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> Result<AssistantImages, ModelsError> {
        let provider = self.get_provider(&model.provider).ok_or_else(|| {
            ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {}", model.provider),
            )
        })?;

        let resolution = self
            .get_auth(
                model,
                Some(&AuthResolutionOverrides {
                    api_key: options.and_then(|o| o.api_key.clone()),
                    env: options.and_then(|o| o.env.clone()),
                    ..Default::default()
                }),
            )
            .await?;
        let Some(resolution) = resolution else {
            // Unconfigured: dispatch as-is; the provider decides what to do.
            return Ok(provider.generate_images(model, context, options).await);
        };
        let auth = resolution.auth;

        let request_model = match auth.base_url {
            Some(base_url) => ImagesModel {
                base_url,
                ..model.clone()
            },
            None => model.clone(),
        };

        // Explicit request options win per-field; headers/env merge per key.
        let api_key = options.and_then(|o| o.api_key.clone()).or(auth.api_key);
        let headers = match (&auth.headers, options.and_then(|o| o.headers.as_ref())) {
            (None, None) => None,
            (base, overrides) => merge_headers(base.as_ref(), overrides),
        };
        let env: Option<ProviderEnv> = match (resolution.env, options.and_then(|o| o.env.clone())) {
            (None, None) => None,
            (base, overrides) => {
                let mut merged = base.unwrap_or_default();
                merged.extend(overrides.unwrap_or_default());
                Some(merged)
            }
        };

        // Upstream always builds an options object
        // (`{ ...options, apiKey, headers, env }`), even when the caller
        // passed none.
        let request_options = {
            let mut merged = options.cloned().unwrap_or_default();
            merged.api_key = api_key;
            merged.headers = headers;
            merged.env = env;
            Some(merged)
        };

        Ok(provider
            .generate_images(&request_model, context, request_options.as_ref())
            .await)
    }
}

/// `createImagesModels`.
pub fn create_images_models(options: Option<CreateModelsOptions>) -> ImagesModels {
    ImagesModels::new(options)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::auth::{ApiKeyAuth, ApiKeyCredential, AuthResult, ModelAuth};
    use crate::types::{
        ImagesApiKind, ImagesOutputContent, ImagesOutputModality, InputModality, ModelCost,
        ModelCostRates, TextContent,
    };

    fn test_image_model(provider: &str, id: &str) -> ImagesModel {
        ImagesModel {
            id: id.to_owned(),
            name: id.to_owned(),
            api: ImagesApiKind::from("test-images"),
            provider: provider.to_owned(),
            base_url: "https://example.test/v1".to_owned(),
            input: vec![InputModality::Text],
            output: vec![ImagesOutputModality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            headers: None,
        }
    }

    fn ok_result(model: &ImagesModel) -> AssistantImages {
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: vec![ImagesOutputContent::Image(crate::types::ImageContent {
                data: "aGk=".to_owned(),
                mime_type: "image/png".to_owned(),
            })],
            response_id: None,
            usage: None,
            stop_reason: ImagesStopReason::Stop,
            error_message: None,
            timestamp: now_ms(),
        }
    }

    struct RecordedCall {
        model: ImagesModel,
        options: Option<ImagesOptions>,
    }

    struct RecordingApi {
        calls: Arc<Mutex<Vec<RecordedCall>>>,
    }

    impl ProviderImages for RecordingApi {
        fn generate_images(
            &self,
            model: &ImagesModel,
            _context: &ImagesContext,
            options: Option<&ImagesOptions>,
        ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(RecordedCall {
                    model: model.clone(),
                    options: options.cloned(),
                });
            let model = model.clone();
            Box::pin(async move { ok_result(&model) })
        }
    }

    #[derive(Clone)]
    struct EnvKeyAuth {
        env_var: &'static str,
    }

    #[async_trait]
    impl ApiKeyAuth for EnvKeyAuth {
        fn name(&self) -> &str {
            "Test key"
        }

        async fn resolve(
            &self,
            ctx: &dyn AuthContext,
            credential: Option<&ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            let key = if let Some(key) = credential.and_then(|c| c.key.clone()) {
                Some(key)
            } else {
                ctx.env(self.env_var).await
            };
            match key {
                Some(key) => Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: credential
                        .map(|_| "stored".to_owned())
                        .or_else(|| Some(self.env_var.to_owned())),
                })),
                None => Ok(None),
            }
        }
    }

    struct FakeAuthContext {
        env: HashMap<String, String>,
    }

    #[async_trait]
    impl AuthContext for FakeAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }

        async fn file_exists(&self, path: &str) -> bool {
            let _ = path;
            false
        }
    }

    fn test_provider(
        id: &str,
        env_var: Option<&'static str>,
        calls: Option<Arc<Mutex<Vec<RecordedCall>>>>,
    ) -> Arc<dyn ImagesProvider> {
        let api = calls.map(|calls| Arc::new(RecordingApi { calls }) as Arc<dyn ProviderImages>);
        create_images_provider(CreateImagesProviderOptions {
            id: id.to_owned(),
            name: None,
            auth: ProviderAuth {
                api_key: env_var
                    .map(|env_var| Arc::new(EnvKeyAuth { env_var }) as Arc<dyn ApiKeyAuth>),
                oauth: None,
            },
            models: vec![test_image_model(id, "model-a")],
            refresh_models: None,
            api: api.unwrap_or_else(|| Arc::new(OkApi)),
        })
    }

    struct OkApi;

    impl ProviderImages for OkApi {
        fn generate_images(
            &self,
            model: &ImagesModel,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
        ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'static>> {
            let model = model.clone();
            Box::pin(async move { ok_result(&model) })
        }
    }

    fn context() -> ImagesContext {
        ImagesContext {
            input: vec![crate::types::ImagesInputContent::Text(TextContent {
                text: "a red circle".to_owned(),
                text_signature: None,
            })],
        }
    }

    #[test]
    fn registers_providers_and_reads_models_synchronously() {
        let models = create_images_models(None);
        models.set_provider(create_images_provider(CreateImagesProviderOptions {
            id: "p1".to_owned(),
            name: None,
            auth: ProviderAuth::default(),
            models: vec![test_image_model("p1", "m1"), test_image_model("p1", "m2")],
            refresh_models: None,
            api: Arc::new(OkApi),
        }));
        models.set_provider(test_provider("p2", None, None));

        let ids: Vec<String> = models
            .get_providers()
            .iter()
            .map(|p| p.id().to_owned())
            .collect();
        assert_eq!(ids, ["p1", "p2"]);
        let model_ids: Vec<String> = models
            .get_models(None)
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(model_ids, ["m1", "m2", "model-a"]);
        let p1_ids: Vec<String> = models
            .get_models(Some("p1"))
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(p1_ids, ["m1", "m2"]);
        assert_eq!(
            models.get_model("p2", "model-a").map(|m| m.id),
            Some("model-a".to_owned())
        );
        assert!(models.get_model("p2", "missing").is_none());

        models.delete_provider("p1");
        assert!(models.get_provider("p1").is_none());
    }

    #[tokio::test]
    async fn resolves_auth_and_merges_it_into_requests_explicit_options_win() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let models = create_images_models(Some(CreateModelsOptions {
            credentials: None,
            auth_context: Some(Arc::new(FakeAuthContext {
                env: [("TEST_KEY".to_owned(), "env-key".to_owned())].into(),
            })),
            models_store: None,
        }));
        models.set_provider(test_provider(
            "p1",
            Some("TEST_KEY"),
            Some(Arc::clone(&calls)),
        ));
        let model = models.get_model("p1", "model-a").expect("model");

        let auth = models.get_auth(&model, None).await.expect("auth");
        assert_eq!(
            auth.as_ref()
                .and_then(|a| a.auth.api_key.clone())
                .as_deref(),
            Some("env-key")
        );
        let auth = models
            .get_auth_for_provider("p1", None)
            .await
            .expect("auth");
        assert_eq!(
            auth.as_ref()
                .and_then(|a| a.auth.api_key.clone())
                .as_deref(),
            Some("env-key")
        );
        let auth = models
            .get_auth(
                &model,
                Some(&AuthResolutionOverrides {
                    api_key: Some("explicit-key".to_owned()),
                    ..Default::default()
                }),
            )
            .await
            .expect("auth");
        assert_eq!(
            auth.as_ref()
                .and_then(|a| a.auth.api_key.clone())
                .as_deref(),
            Some("explicit-key")
        );

        let result = models.generate_images(&model, &context(), None).await;
        assert_eq!(result.stop_reason, ImagesStopReason::Stop);
        {
            let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(calls[0].model.id, "model-a");
            assert_eq!(
                calls[0]
                    .options
                    .as_ref()
                    .and_then(|o| o.api_key.clone())
                    .as_deref(),
                Some("env-key")
            );
        }

        models
            .generate_images(
                &model,
                &context(),
                Some(&ImagesOptions {
                    api_key: Some("explicit".to_owned()),
                    ..Default::default()
                }),
            )
            .await;
        {
            let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(
                calls[1]
                    .options
                    .as_ref()
                    .and_then(|o| o.api_key.clone())
                    .as_deref(),
                Some("explicit")
            );
        }
    }

    #[tokio::test]
    async fn merges_provider_resolved_env_into_image_options() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let models = create_images_models(None);
        models.set_provider(create_images_provider(CreateImagesProviderOptions {
            id: "p1".to_owned(),
            name: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(FixedKeyAuth)),
                oauth: None,
            },
            models: vec![test_image_model("p1", "model-a")],
            refresh_models: None,
            api: Arc::new(RecordingApi {
                calls: Arc::clone(&calls),
            }),
        }));
        let model = models.get_model("p1", "model-a").expect("model");

        models
            .generate_images(
                &model,
                &context(),
                Some(&ImagesOptions {
                    api_key: Some("request-key".to_owned()),
                    env: Some(
                        [
                            ("REQUEST_ONLY".to_owned(), "request".to_owned()),
                            ("SHARED".to_owned(), "request".to_owned()),
                        ]
                        .into(),
                    ),
                    ..Default::default()
                }),
            )
            .await;

        let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
        let options = calls[0].options.as_ref().expect("options");
        assert_eq!(options.api_key.as_deref(), Some("request-key"));
        let env = options.env.as_ref().expect("env");
        assert_eq!(env.get("PROVIDER_ONLY"), Some(&"provider".to_owned()));
        assert_eq!(env.get("REQUEST_ONLY"), Some(&"request".to_owned()));
        assert_eq!(env.get("SHARED"), Some(&"request".to_owned()));
    }

    struct FixedKeyAuth;

    #[async_trait]
    impl ApiKeyAuth for FixedKeyAuth {
        fn name(&self) -> &str {
            "Test key"
        }

        async fn resolve(
            &self,
            _ctx: &dyn AuthContext,
            _credential: Option<&ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("provider-key".to_owned()),
                    headers: None,
                    base_url: None,
                },
                env: Some(
                    [
                        ("PROVIDER_ONLY".to_owned(), "provider".to_owned()),
                        ("SHARED".to_owned(), "provider".to_owned()),
                    ]
                    .into(),
                ),
                source: None,
            }))
        }
    }

    #[tokio::test]
    async fn error_result_for_unknown_providers_and_unconfigured_auth() {
        let models = create_images_models(Some(CreateModelsOptions {
            credentials: None,
            auth_context: Some(Arc::new(FakeAuthContext {
                env: HashMap::new(),
            })),
            models_store: None,
        }));
        let ghost = test_image_model("ghost", "m");
        let result = models.generate_images(&ghost, &context(), None).await;
        assert_eq!(result.stop_reason, ImagesStopReason::Error);
        assert!(
            result
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("Unknown provider: ghost")
        );

        // Unconfigured (resolve -> None) still dispatches; the provider decides.
        let calls = Arc::new(Mutex::new(Vec::new()));
        models.set_provider(test_provider(
            "p1",
            Some("MISSING"),
            Some(Arc::clone(&calls)),
        ));
        let model = models.get_model("p1", "model-a").expect("model");
        assert!(models.get_auth(&model, None).await.expect("auth").is_none());
        models.generate_images(&model, &context(), None).await;
        let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            calls[0].options.as_ref().and_then(|o| o.api_key.clone()),
            None
        );
    }

    #[tokio::test]
    async fn dynamic_providers_refresh_with_inflight_dedupe() {
        let fetches = Arc::new(AtomicUsize::new(0));
        let fetch_count = Arc::clone(&fetches);
        let dynamic = create_images_provider(CreateImagesProviderOptions {
            id: "dyn".to_owned(),
            name: None,
            auth: ProviderAuth::default(),
            models: Vec::new(),
            refresh_models: Some(Arc::new(move || {
                let fetch_count = Arc::clone(&fetch_count);
                Box::pin(async move {
                    fetch_count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Ok(vec![test_image_model("dyn", "listed")])
                })
            })),
            api: Arc::new(OkApi),
        });
        let models = create_images_models(None);
        models.set_provider(dynamic);

        assert!(models.get_models(Some("dyn")).is_empty());
        let (a, b) = tokio::join!(models.refresh(Some("dyn")), models.refresh(Some("dyn")));
        a.expect("refresh a");
        b.expect("refresh b");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert!(models.get_model("dyn", "listed").is_some());

        // Failures reject with ModelsError "model_source" for a single provider.
        let flaky = create_images_provider(CreateImagesProviderOptions {
            id: "flaky".to_owned(),
            name: None,
            auth: ProviderAuth::default(),
            models: Vec::new(),
            refresh_models: Some(Arc::new(|| {
                Box::pin(async move { Err("fetch failed".to_owned()) })
            })),
            api: Arc::new(OkApi),
        });
        models.set_provider(flaky);
        let error = models.refresh(Some("flaky")).await.expect_err("rejects");
        assert_eq!(error.code, ModelsErrorCode::ModelSource);
        models.refresh(None).await.expect("all best-effort");
    }
}
