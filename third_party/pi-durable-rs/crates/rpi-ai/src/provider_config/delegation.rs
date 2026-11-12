//! Provider delegation preserving native filters, refresh, and stream routing.
//! Used by models.json provider composition, without application state.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use std::sync::Arc;
use futures::future::BoxFuture;
use crate::auth::{Credential, ModelsError, ProviderAuth};
use crate::models::{Provider, RefreshModelsContext};
use crate::types::{AnyModel, Model, ProviderHeaders, SimpleStreamOptions, StreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;
use super::catalog::ComposedModels;
use super::{api_streams, unsupported_api_stream};

pub(super) struct ModelOverridingProvider {
    pub(super) base: Arc<dyn Provider>,
    pub(super) composed: ComposedModels,
    pub(super) name: String,
    pub(super) base_url: Option<String>,
    pub(super) auth: Option<ProviderAuth>,
}

impl ModelOverridingProvider {
    fn base_supports(&self, model: &Model) -> bool {
        self.base
            .get_models()
            .iter()
            .any(|entry| entry.api == model.api)
    }
}

impl Provider for ModelOverridingProvider {
    fn id(&self) -> &str {
        self.base.id()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref().or_else(|| self.base.base_url())
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.base.headers()
    }

    fn auth(&self) -> &ProviderAuth {
        self.auth.as_ref().unwrap_or_else(|| self.base.auth())
    }

    fn get_models(&self) -> Vec<Model> {
        self.composed.get_models()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        self.base.filter_models(models, credential)
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        self.base.filter_all_models(models, credential)
    }

    fn refresh_models(
        &self,
        context: RefreshModelsContext,
    ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
        self.base.refresh_models(context)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        if self.base_supports(model) {
            return self.base.stream(model, context, options);
        }
        match api_streams(model.api.as_str()) {
            Some(streams) => streams.stream(model, context, options),
            None => unsupported_api_stream(model),
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        if self.base_supports(model) {
            return self.base.stream_simple(model, context, options);
        }
        match api_streams(model.api.as_str()) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => Err(format!(
                "No API provider registered for api: {}",
                model.api.as_str()
            )),
        }
    }
}

pub(super) struct RefreshDelegatingProvider {
    pub(super) base: Arc<dyn Provider>,
    pub(super) inner: Arc<dyn Provider>,
    pub(super) composed: ComposedModels,
}

impl RefreshDelegatingProvider {
    fn base_supports(&self, model: &Model) -> bool {
        self.base
            .get_models()
            .iter()
            .any(|entry| entry.api == model.api)
    }
}

impl Provider for RefreshDelegatingProvider {
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
        self.composed.get_models()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        self.base.filter_models(models, credential)
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        self.base.filter_all_models(models, credential)
    }

    fn refresh_models(
        &self,
        context: RefreshModelsContext,
    ) -> Option<BoxFuture<'_, Result<(), ModelsError>>> {
        self.base.refresh_models(context)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        if self.base_supports(model) {
            return self.base.stream(model, context, options);
        }
        match api_streams(model.api.as_str()) {
            Some(streams) => streams.stream(model, context, options),
            None => unsupported_api_stream(model),
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        if self.base_supports(model) {
            return self.base.stream_simple(model, context, options);
        }
        match api_streams(model.api.as_str()) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => Err(format!(
                "No API provider registered for api: {}",
                model.api.as_str()
            )),
        }
    }
