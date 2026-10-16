//! The models hub's list rows for one view, kind tab, and search query, built
//! from the catalog as pure data: which models a view lists, their OMP sort
//! order, their search ranking, and the metric cells each row shows. The
//! component renders these rows and applies the key ladder to them.

use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelEntry, ModelsCatalog};
use roost_protocol::wire::agent_chat::{model_search_text, rank_matches};

use super::hub_format::{
    format_context, format_cost_pair, format_intelligence, format_tokens_per_second,
};
use super::hub_logic::HubView;
use super::hub_order::{OrderKey, sort_order_keys};
use crate::components::settings::agent_roles::{ROLES, WEB_ENGINES};

/// OMP's kind tabs, in tab order.
pub const MODEL_KIND_TABS: [&str; 10] = [
    "chat",
    "tiny",
    "image",
    "tts",
    "stt",
    "search",
    "judge",
    "embedding",
    "rerank",
    "video",
];

/// One model row, with everything its cells and detail pane show.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseRow {
    pub selector: String,
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub kind: String,
    pub recommended: bool,
    pub context: String,
    pub intelligence: String,
    pub speed: String,
    pub cost: String,
    /// Thinking-capable; the detail pane lists `reasoning` among its facts.
    pub reasoning: bool,
    /// Accepts image input.
    pub images: bool,
    /// The roles this model leads, in role order (`default`, `smol`, …).
    pub roles: Vec<String>,
    /// Set on `web/<engine>` rows, which are not catalog models.
    pub web: bool,
}

/// The model kind a catalog entry is listed under: its declared kind, else
/// `judge` for a judgment model, else `chat`.
pub fn kind_of(entry: &ModelEntry) -> &str {
    if !entry.kind.is_empty() {
        entry.kind.as_str()
    } else if entry.classifier {
        "judge"
    } else {
        "chat"
    }
}

/// The role's leader selector: the chain's first entry without its `:level`.
pub fn role_leader(chain: &str) -> Option<String> {
    let leader = chain.split(',').next()?.trim();
    let selector = leader.split(':').next().unwrap_or(leader).trim();
    (!selector.is_empty()).then(|| selector.to_owned())
}

/// The roles each selector leads, in `ROLES` order.
pub fn roles_led_by(settings: &AgentSettingsView) -> Vec<(String, Vec<String>)> {
    let mut led: Vec<(String, Vec<String>)> = Vec::new();
    for (role, _, _, _) in ROLES {
        let Some(chain) = settings.model_roles.get(role) else {
            continue;
        };
        let Some(selector) = role_leader(chain) else {
            continue;
        };
        match led.iter_mut().find(|(existing, _)| *existing == selector) {
            Some((_, roles)) => roles.push(role.to_owned()),
            None => led.push((selector, vec![role.to_owned()])),
        }
    }
    led
}

/// Rows for `view` in `kind`, filtered and ranked by `query`, in OMP order.
pub fn browse_rows(
    catalog: &ModelsCatalog,
    settings: &AgentSettingsView,
    view: &HubView,
    kind: &str,
    query: &str,
) -> Vec<BrowseRow> {
    let led = roles_led_by(settings);
    let recent: Vec<String> = catalog
        .recent_models
        .iter()
        .map(|model| format!("{}/{}", model.provider, model.model_id))
        .collect();
    let mut rows: Vec<BrowseRow> = catalog
        .models
        .iter()
        .filter(|entry| entry.available && kind_of(entry) == kind)
        .filter(|entry| match view {
            HubView::Recent => recent
                .iter()
                .any(|selector| *selector == format!("{}/{}", entry.provider, entry.model_id)),
            HubView::Provider(provider) => entry.provider == *provider,
            HubView::All | HubView::Roles => true,
        })
        .map(|entry| row_from_entry(entry, &led))
        .collect();
    if kind == "search" && matches!(view, HubView::All) {
        rows.extend(
            WEB_ENGINES
                .iter()
                .map(|(selector, name)| web_row(selector, name)),
        );
    }
    if !query.is_empty() {
        let haystacks: Vec<String> = rows
            .iter()
            .map(|row| model_search_text(&row.selector, &row.name))
            .collect();
        let order = rank_matches(&haystacks, query);
        return order.into_iter().map(|index| rows[index].clone()).collect();
    }
    let mut keys: Vec<OrderKey<'_>> = rows
        .iter()
        .map(|row| OrderKey {
            selector: &row.selector,
            provider: &row.provider,
            id: &row.model_id,
            role_rank: row
                .roles
                .first()
                .and_then(|role| ROLES.iter().position(|(name, _, _, _)| name == role)),
            mru_index: recent.iter().position(|selector| *selector == row.selector),
            priority: catalog
                .models
                .iter()
                .find(|entry| format!("{}/{}", entry.provider, entry.model_id) == row.selector)
                .and_then(|entry| entry.priority),
        })
        .collect();
    sort_order_keys(&mut keys, false);
    let order: Vec<String> = keys.iter().map(|key| key.selector.to_owned()).collect();
    order
        .iter()
        .filter_map(|selector| rows.iter().find(|row| row.selector == *selector).cloned())
        .collect()
}

fn row_from_entry(entry: &ModelEntry, led: &[(String, Vec<String>)]) -> BrowseRow {
    let selector = format!("{}/{}", entry.provider, entry.model_id);
    let roles = led
        .iter()
        .find(|(leader, _)| *leader == selector)
        .map(|(_, roles)| roles.clone())
        .unwrap_or_default();
    BrowseRow {
        kind: kind_of(entry).to_owned(),
        context: format_context(entry.context_window),
        intelligence: format_intelligence(entry.intelligence),
        speed: format_tokens_per_second(entry.tokens_per_second),
        cost: format_cost_pair(
            entry.input_cost_micros,
            entry.output_cost_micros,
            entry.pricing_status.as_deref(),
        ),
        selector,
        provider: entry.provider.clone(),
        model_id: entry.model_id.clone(),
        name: entry.name.clone(),
        recommended: entry.recommended,
        reasoning: entry.reasoning,
        images: entry.images,
        roles,
        web: false,
    }
}

fn web_row(selector: &str, name: &str) -> BrowseRow {
    let model_id = selector.trim_start_matches("web/").to_owned();
    BrowseRow {
        selector: selector.to_owned(),
        provider: "web".to_owned(),
        model_id,
        name: name.to_owned(),
        kind: "search".to_owned(),
        recommended: false,
        context: String::new(),
        intelligence: String::new(),
        speed: String::new(),
        cost: "free".to_owned(),
        reasoning: false,
        images: false,
        roles: Vec::new(),
        web: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_leader_drops_level_and_fallbacks() {
        assert_eq!(
            role_leader("openai/gpt-5:high,anthropic/x"),
            Some("openai/gpt-5".to_owned())
        );
        assert_eq!(role_leader(""), None);
    }

    #[test]
    fn untagged_entries_are_chat_and_judgment_is_judge() {
        let chat = ModelEntry {
            provider: "p".into(),
            model_id: "m".into(),
            ..Default::default()
        };
        let judge = ModelEntry {
            classifier: true,
            ..chat.clone()
        };
        assert_eq!(kind_of(&chat), "chat");
        assert_eq!(kind_of(&judge), "judge");
    }

    #[test]
    fn roles_group_under_their_leader() {
        let mut settings = AgentSettingsView::default();
        settings.model_roles.insert("default".into(), "p/a".into());
        settings
            .model_roles
            .insert("smol".into(), "p/a:low,p/b".into());
        let led = roles_led_by(&settings);
        assert_eq!(
            led,
            vec![(
                "p/a".to_owned(),
                vec!["default".to_owned(), "smol".to_owned()]
            )]
        );
    }
}
