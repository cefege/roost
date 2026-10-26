//! Port of `packages/ai/src/providers/typesafe.ts` @ abe508e1b — TypeSafe,
//! classifier-only provider (schema v6). The provider registers the
//! `typesafe-system-one` classifier implementation; it has no chat models and
//! no dynamic catalog.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::typesafe_system_one::{TYPESAFE_SYSTEM_ONE_API, typesafe_system_one_api};
use crate::auth::{ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_all_models;
use crate::models::{CreateProviderOptions, Provider, create_provider};

/// `typesafeProvider()`.
pub fn typesafe_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        name: Some("TypeSafe".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "TypeSafe API key",
                &["TYPESAFE_API_KEY"],
            ))),
            oauth: None,
        },
        // Classifier-only: the chat list stays empty; the schema-v6 all-type
        // catalog carries the Jev entries.
        models: Vec::new(),
        all_models: get_builtin_all_models("typesafe").to_vec(),
        classifiers: HashMap::from([(
            TYPESAFE_SYSTEM_ONE_API.to_owned(),
            typesafe_system_one_api(),
        )]),
        ..Default::default()
    })
}
