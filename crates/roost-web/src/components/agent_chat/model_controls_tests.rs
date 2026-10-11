//! Chat readiness for the model controls, independent of connected judge accounts.
//! Compiled by model_controls under cfg(test); catalog entries are the public
//! input that decides whether a user may submit a normal chat prompt.

use super::ModelSetup;
use roost_protocol::wire::agent_chat::{ModelEntry, ModelsCatalog};

#[test]
fn a_classifier_account_does_not_enable_chat() {
    let catalog = ModelsCatalog {
        models: vec![ModelEntry {
            provider: "typesafe".into(),
            model_id: "jev-latest".into(),
            name: "System One".into(),
            reasoning: false,
            available: true,
            classifier: true,
        }],
        providers: Vec::new(),
        thinking_levels: Vec::new(),
        default_model: None,
    };
    assert_eq!(
        ModelSetup::from_catalog(Some(&catalog), false),
        ModelSetup::NoProvider
    );
}
