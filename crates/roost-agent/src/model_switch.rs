//! `/model <x>` and `/switch <x>`: the exact selector grammar first, then a
//! fuzzy match over the available chat models, newest first.

use std::collections::BTreeMap;

use roost_llm::{ModelKind, THINKING_LEVELS};
use roost_protocol::wire::agent_chat::{compare_newest_first, model_search_text, rank_matches};

use crate::error::AgentError;
use crate::records::AgentSettings;
use crate::roles::{self, ResolvedModel};
use crate::traits::Llm;

/// Roles (`@x`) and chains (`a, b`) keep the exact grammar and its errors; a
/// single name that the grammar cannot resolve falls through to fuzzy search.
pub(crate) async fn resolve_session_selector(
    llm: &dyn Llm,
    settings: &AgentSettings,
    text: &str,
) -> Result<Option<ResolvedModel>, AgentError> {
    let text = text.trim();
    if text.starts_with('@') || text.contains(',') {
        return roles::resolve_selector(llm, settings, text).await;
    }
    if let Ok(Some(found)) = roles::resolve_selector(llm, settings, text).await {
        return Ok(Some(found));
    }
    let (query, thinking) = match text.rsplit_once(':') {
        Some((query, level)) if level == "auto" || THINKING_LEVELS.contains(&level) => {
            (query.trim(), Some(level.to_owned()))
        }
        _ => (text, None),
    };
    if query.is_empty() {
        return Ok(None);
    }
    let mut credentials: BTreeMap<&str, bool> = BTreeMap::new();
    let mut candidates = Vec::new();
    for model in llm.catalog().models() {
        if model.kind != ModelKind::Chat {
            continue;
        }
        let available = match credentials.get(model.provider.as_str()) {
            Some(known) => *known,
            None => {
                let found = llm.has_credential(&model.provider).await;
                credentials.insert(&model.provider, found);
                found
            }
        };
        if available {
            candidates.push(model);
        }
    }
    candidates
        .sort_by(|left, right| compare_newest_first(&left.name, &left.id, &right.name, &right.id));
    let haystacks: Vec<String> = candidates
        .iter()
        .map(|model| model_search_text(&format!("{}/{}", model.provider, model.id), &model.name))
        .collect();
    Ok(rank_matches(&haystacks, query)
        .first()
        .map(|index| ResolvedModel {
            info: candidates[*index].clone(),
            thinking,
        }))
}
