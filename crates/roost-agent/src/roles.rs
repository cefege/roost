//! Ported from oh-my-pi packages/coding-agent/src/config/model-resolver.ts and src/judgment/index.ts (MIT).
//! Model selectors (`provider/id[:thinking]`, `@role`, comma lists) and role
//! resolution: configured value first, then the role's fallback. "Available"
//! means the catalog has the model and the account pool holds a credential.

use roost_llm::{ModelInfo, ModelKind, THINKING_LEVELS, WireApi};

use crate::error::AgentError;
use crate::records::{AgentSettings, Role};
use crate::traits::Llm;

/// The judge chain when no `judge` role is configured: TypeSafe's Jev
/// directly, then through OpenRouter, then progressively larger chat models.
const BUILTIN_JUDGE_CHAIN: &str =
    "typesafe/jev-latest, openrouter/~typesafe/jev-latest, @tiny, @smol, @default";

/// Chat models tried, in order, when neither the `default` role nor a
/// default model is configured.
const DEFAULT_MODEL_PREFERENCES: [(&str, &str); 4] = [
    ("anthropic", "claude-sonnet-5-5"),
    ("anthropic", "claude-sonnet-5"),
    ("openai-codex", "gpt-6.1-sol"),
    ("openai-codex", "gpt-5.5"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorCandidate {
    Model {
        provider: String,
        id: String,
        thinking: Option<String>,
    },
    Role(Role),
}

/// A model chosen for a call, with the thinking level its selector pinned.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub info: ModelInfo,
    pub thinking: Option<String>,
}

/// Parses a selector. Every comma-separated candidate must be well formed.
pub fn parse_selector(text: &str) -> Result<Vec<SelectorCandidate>, AgentError> {
    let mut candidates = Vec::new();
    for raw in text.split(',') {
        let candidate = raw.trim();
        if candidate.is_empty() {
            return Err(AgentError::InvalidArgument(format!(
                "empty candidate in model selector {text:?}"
            )));
        }
        candidates.push(parse_candidate(candidate)?);
    }
    if candidates.is_empty() {
        return Err(AgentError::InvalidArgument("empty model selector".into()));
    }
    Ok(candidates)
}

/// Syntax check for a configured role value.
pub fn validate_selector(text: &str) -> Result<(), AgentError> {
    parse_selector(text).map(|_| ())
}

fn parse_candidate(candidate: &str) -> Result<SelectorCandidate, AgentError> {
    if let Some(role_name) = candidate.strip_prefix('@') {
        return Role::from_name(role_name)
            .map(SelectorCandidate::Role)
            .ok_or_else(|| AgentError::InvalidArgument(format!("unknown role @{role_name}")));
    }
    let Some((provider, rest)) = candidate.split_once('/') else {
        return Err(AgentError::InvalidArgument(format!(
            "model selector {candidate:?} must be provider/id or @role"
        )));
    };
    // Model ids may contain `:` (`respan/span-01-lite:free`), so only a known
    // thinking level after the last `:` is a thinking suffix.
    let (id, thinking) = match rest.rsplit_once(':') {
        Some((id, level)) if level == "auto" || THINKING_LEVELS.contains(&level) => {
            (id, Some(level.to_owned()))
        }
        _ => (rest, None),
    };
    let valid_provider = !provider.is_empty()
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !valid_provider || id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(AgentError::InvalidArgument(format!(
            "model selector {candidate:?} must be provider/id or @role"
        )));
    }
    Ok(SelectorCandidate::Model {
        provider: provider.to_owned(),
        id: id.to_owned(),
        thinking,
    })
}

/// Whether a model answers judgments natively (System One) rather than by prompting.
pub fn is_native_judge(model: &ModelInfo) -> bool {
    model.api == WireApi::TypesafeSystemOne
}

/// Resolves an explicit selector (e.g. `/model`), first available candidate wins.
pub async fn resolve_selector(
    llm: &dyn Llm,
    settings: &AgentSettings,
    text: &str,
) -> Result<Option<ResolvedModel>, AgentError> {
    let candidates = parse_selector(text)?;
    let mut visited = Vec::new();
    Ok(first_available(llm, settings, &candidates, &mut visited).await)
}

/// Resolves a role to a model, following the role's fallback.
pub async fn resolve_role(
    llm: &dyn Llm,
    settings: &AgentSettings,
    role: Role,
) -> Option<ResolvedModel> {
    let mut visited = Vec::new();
    resolve_role_inner(llm, settings, role, &mut visited).await
}

/// The judge candidates in attempt order. From the first native candidate on
/// only native candidates remain: a prompted model never stands in for a
/// failed native judgment, whose calibrated probabilities it cannot reproduce.
/// Without a native candidate the session model closes the chain.
pub async fn judge_candidates(
    llm: &dyn Llm,
    settings: &AgentSettings,
    session_model: Option<&ModelInfo>,
) -> Vec<ModelInfo> {
    let configured = settings.model_roles.get(&Role::Judge).map(String::as_str);
    let selector = configured.unwrap_or(BUILTIN_JUDGE_CHAIN);
    let Ok(candidates) = parse_selector(selector) else {
        return Vec::new();
    };
    let mut chain: Vec<ModelInfo> = Vec::new();
    for candidate in &candidates {
        let mut visited = vec![Role::Judge];
        let resolved = match candidate {
            SelectorCandidate::Model { .. } => available_model(llm, candidate)
                .await
                .map(|model| model.info),
            SelectorCandidate::Role(role) => {
                Box::pin(resolve_role_inner(llm, settings, *role, &mut visited))
                    .await
                    .map(|model| model.info)
            }
        };
        if let Some(model) = resolved
            && !chain.iter().any(|known| same_model(known, &model))
        {
            chain.push(model);
        }
    }
    if let Some(first_native) = chain.iter().position(is_native_judge) {
        let mut index = 0;
        chain.retain(|model| {
            let keep = index < first_native || is_native_judge(model);
            index += 1;
            keep
        });
        return chain;
    }
    if let Some(session) = session_model
        && !chain.iter().any(|known| same_model(known, session))
    {
        chain.push(session.clone());
    }
    chain
}

async fn resolve_role_inner(
    llm: &dyn Llm,
    settings: &AgentSettings,
    role: Role,
    visited: &mut Vec<Role>,
) -> Option<ResolvedModel> {
    if visited.contains(&role) {
        return None;
    }
    visited.push(role);
    if role == Role::Judge {
        return judge_candidates(llm, settings, None)
            .await
            .into_iter()
            .next()
            .map(|info| ResolvedModel {
                info,
                thinking: None,
            });
    }
    if let Some(configured) = settings.model_roles.get(&role) {
        let parsed = parse_selector(configured);
        if let Ok(candidates) = &parsed
            && let Some(found) = Box::pin(first_available(llm, settings, candidates, visited)).await
        {
            return Some(found);
        }
        // An explicitly configured advisor that cannot resolve stays off
        // rather than silently reviewing with a different model.
        if role == Role::Advisor {
            return None;
        }
    }
    match role {
        Role::Tiny => Box::pin(resolve_role_inner(llm, settings, Role::Smol, visited)).await,
        Role::Smol | Role::Slow => {
            Box::pin(resolve_role_inner(llm, settings, Role::Default, visited)).await
        }
        Role::Advisor => {
            if settings.model_roles.contains_key(&Role::Slow)
                && let Some(found) =
                    Box::pin(resolve_role_inner(llm, settings, Role::Slow, visited)).await
            {
                return Some(found);
            }
            Box::pin(resolve_role_inner(llm, settings, Role::Default, visited)).await
        }
        Role::Default => default_model(llm, settings).await,
        Role::Plan | Role::Task | Role::Judge => None,
    }
}

async fn default_model(llm: &dyn Llm, settings: &AgentSettings) -> Option<ResolvedModel> {
    if let Some(reference) = &settings.default_model {
        let candidate = SelectorCandidate::Model {
            provider: reference.provider.clone(),
            id: reference.model_id.clone(),
            thinking: None,
        };
        if let Some(found) = available_model(llm, &candidate).await {
            return Some(found);
        }
    }
    for (provider, id) in DEFAULT_MODEL_PREFERENCES {
        let candidate = SelectorCandidate::Model {
            provider: provider.to_owned(),
            id: id.to_owned(),
            thinking: None,
        };
        if let Some(found) = available_model(llm, &candidate).await {
            return Some(found);
        }
    }
    for model in llm.catalog().models() {
        if model.kind == ModelKind::Chat && llm.has_credential(&model.provider).await {
            return Some(ResolvedModel {
                info: model.clone(),
                thinking: None,
            });
        }
    }
    None
}

async fn first_available(
    llm: &dyn Llm,
    settings: &AgentSettings,
    candidates: &[SelectorCandidate],
    visited: &mut Vec<Role>,
) -> Option<ResolvedModel> {
    for candidate in candidates {
        let found = match candidate {
            SelectorCandidate::Model { .. } => available_model(llm, candidate).await,
            SelectorCandidate::Role(role) => {
                Box::pin(resolve_role_inner(llm, settings, *role, visited)).await
            }
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

async fn available_model(llm: &dyn Llm, candidate: &SelectorCandidate) -> Option<ResolvedModel> {
    let SelectorCandidate::Model {
        provider,
        id,
        thinking,
    } = candidate
    else {
        return None;
    };
    let info = llm.catalog().get(provider, id)?.clone();
    if !llm.has_credential(provider).await {
        return None;
    }
    Some(ResolvedModel {
        info,
        thinking: thinking.clone(),
    })
}

fn same_model(left: &ModelInfo, right: &ModelInfo) -> bool {
    left.provider == right.provider && left.id == right.id
}
