//! The models hub's views, rows and model completions, from the catalog and
//! settings the coordinator returns.

use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ModelEntry, ModelRef, ModelsCatalog, ProviderEntry,
};
use roost_web::components::agent_chat::model_hub::hub_logic::{
    HubSection, HubView, assigned_roles, format_cost_pair, format_tokens, hub_options,
    hub_sections, initial_view, model_argument_values, model_facts,
};
use roost_web::components::agent_chat::model_picker::{PickerOption, filter_options, superseded};

fn entry(provider: &str, model_id: &str, name: &str, available: bool) -> ModelEntry {
    ModelEntry {
        provider: provider.into(),
        model_id: model_id.into(),
        name: name.into(),
        available,
        ..ModelEntry::default()
    }
}

fn provider(id: &str, name: &str, configured: bool, category: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        name: name.into(),
        configured,
        category: category.into(),
        accounts: Vec::new(),
        supports_oauth: false,
    }
}

fn model_ref(provider: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider: provider.into(),
        model_id: model_id.into(),
    }
}

fn sonnet_5_5() -> ModelEntry {
    ModelEntry {
        reasoning: true,
        images: true,
        context_window: 1_000_000,
        input_cost_micros: 2_000_000,
        output_cost_micros: 10_000_000,
        ..entry("anthropic", "claude-sonnet-5-5", "Claude Sonnet 5.5", true)
    }
}

fn catalog() -> ModelsCatalog {
    ModelsCatalog {
        models: vec![
            sonnet_5_5(),
            entry("anthropic", "claude-sonnet-5", "Claude Sonnet 5", true),
            entry("openai-codex", "gpt-5", "GPT-5", false),
            entry("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5", true),
            ModelEntry {
                classifier: true,
                ..entry("typesafe", "jev-latest", "System One", true)
            },
        ],
        providers: vec![
            provider("anthropic", "Anthropic", true, "model"),
            provider("openai-codex", "OpenAI Codex", false, "model"),
            provider("typesafe", "TypeSafe", true, "model"),
            provider("brave", "Brave", false, "web_search"),
        ],
        thinking_levels: vec!["off".into(), "low".into(), "high".into()],
        default_model: None,
        recent_models: vec![
            model_ref("openai-codex", "gpt-5"),
            model_ref("anthropic", "claude-sonnet-5"),
        ],
    }
}

fn section(view: HubView, label: &str, count: Option<usize>, locked: bool) -> HubSection {
    HubSection {
        view,
        label: label.into(),
        count,
        locked,
    }
}

#[test]
fn sections_list_recent_roles_all_then_signed_in_and_locked_providers() {
    let catalog = catalog();
    assert_eq!(
        hub_sections(&catalog),
        [
            section(HubView::Recent, "Recent", Some(1), false),
            section(HubView::Roles, "Roles", None, false),
            section(HubView::All, "All models", Some(3), false),
            section(
                HubView::Provider("anthropic".into()),
                "Anthropic",
                Some(3),
                false
            ),
            section(
                HubView::Provider("openai-codex".into()),
                "OpenAI Codex",
                Some(1),
                true
            ),
        ]
    );
    assert_eq!(initial_view(&catalog), HubView::Recent);
    let fresh = ModelsCatalog {
        recent_models: Vec::new(),
        ..catalog
    };
    assert_eq!(hub_sections(&fresh)[0].view, HubView::Roles);
    assert_eq!(initial_view(&fresh), HubView::All);
}

#[test]
fn recent_rows_keep_reachable_models_and_name_their_roles() {
    let mut settings = AgentSettingsView::default();
    settings.model_roles.insert(
        "default".into(),
        "anthropic/claude-sonnet-5:high, anthropic/claude-haiku-4-5".into(),
    );
    settings
        .model_roles
        .insert("smol".into(), "anthropic/claude-haiku-4-5".into());
    let rows = hub_options(&catalog(), &HubView::Recent, &settings);
    let selectors: Vec<&str> = rows.iter().map(|row| row.selector.as_str()).collect();
    assert_eq!(selectors, ["anthropic/claude-sonnet-5"]);
    assert_eq!(rows[0].group, "");
    let all = hub_options(&catalog(), &HubView::All, &settings);
    assert_eq!(all[0].group, "Anthropic");
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("anthropic/claude-sonnet-5 · roles: default")
    );
    assert_eq!(
        assigned_roles(&settings, "anthropic/claude-haiku-4-5", &[]),
        ["smol"]
    );
}

#[test]
fn model_facts_read_like_a_spec_line() {
    assert_eq!(
        model_facts(&sonnet_5_5()),
        "anthropic/claude-sonnet-5-5 · 1M ctx · $2/10 · thinking · images"
    );
    let tokens: Vec<String> = [1_000_000, 1_048_576, 1_500_000, 200_000, 512]
        .into_iter()
        .map(format_tokens)
        .collect();
    assert_eq!(tokens, ["1M", "1M", "1.5M", "200k", "512"]);
    assert_eq!(format_cost_pair(0, 0), None);
    assert_eq!(
        format_cost_pair(100_000, 500_000).as_deref(),
        Some("$0.1/0.5")
    );
}

#[test]
fn model_completions_offer_roles_ranked_models_or_nothing() {
    let models: Vec<(String, String)> = ('a'..='j')
        .map(|letter| (format!("p/m{letter}"), format!("Model {letter}")))
        .chain([
            (
                "anthropic/claude-haiku-4-5".into(),
                "Claude Haiku 4.5".into(),
            ),
            ("anthropic/claude-sonnet-5".into(), "Claude Sonnet 5".into()),
        ])
        .collect();
    assert!(model_argument_values(&models, "sonnet:").is_empty());
    let roles: Vec<String> = model_argument_values(&models, "@s")
        .into_iter()
        .map(|(value, _)| value)
        .collect();
    assert_eq!(roles, ["@smol", "@slow"]);
    assert_eq!(
        model_argument_values(&models, "sonn")[0].0,
        "anthropic/claude-sonnet-5"
    );
    let blank = model_argument_values(&models, "");
    assert_eq!(blank, models[..8]);
}

#[test]
fn only_the_newest_version_of_each_model_is_offered_first() {
    let pair = |selector: &str, name: &str| (selector.to_owned(), name.to_owned());
    let models = [
        pair("anthropic/claude-sonnet-5-5", "Claude Sonnet 5.5"),
        pair("anthropic/claude-opus-5", "Claude Opus 5"),
        pair("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5"),
        pair(
            "openrouter/anthropic/claude-sonnet-4-5",
            "Anthropic: Claude Sonnet 4.5",
        ),
        pair("anthropic/claude-opus-4-1", "Claude Opus 4.1"),
    ];
    let items = || models.iter().map(|(s, n)| (s.as_str(), n.as_str()));
    assert_eq!(superseded(items(), &[]), [false, false, true, false, true]);
    // A kept older model stays listed; a kept newest one still hides the rest.
    assert_eq!(
        superseded(items(), &["anthropic/claude-sonnet-5-5".to_owned()]),
        [false, false, true, false, true]
    );
    assert_eq!(
        superseded(items(), &["anthropic/claude-opus-4-1".to_owned()]),
        [false, false, true, false, false]
    );
    let values: Vec<String> = model_argument_values(&models, "sonnet")
        .into_iter()
        .map(|(value, _)| value)
        .collect();
    assert_eq!(
        values,
        [
            "anthropic/claude-sonnet-5-5",
            "openrouter/anthropic/claude-sonnet-4-5"
        ]
    );
    // Only an older version matches: it is offered rather than nothing.
    assert_eq!(
        model_argument_values(&models, "4.1")[0].0,
        "anthropic/claude-opus-4-1"
    );
}

#[test]
fn picker_search_matches_scattered_letters() {
    let options = [PickerOption {
        selector: "anthropic/claude-sonnet-5".into(),
        name: "Claude Sonnet 5".into(),
        group: "Anthropic".into(),
        detail: None,
    }];
    assert_eq!(filter_options(&options, "cs5").len(), 1);
    assert!(filter_options(&options, "gpt").is_empty());
}
