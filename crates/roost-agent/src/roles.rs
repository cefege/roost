//! Ported from oh-my-pi packages/coding-agent/src/config/model-resolver.ts,
//! src/judgment/index.ts, and src/priority.json (MIT).
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

/// Default provider chain from oh-my-pi's `src/priority.json`.
pub const DEFAULT_WEB_CHAIN: &str = "web/parallel, web/hosted, web/exa, web/firecrawl, web/searxng, web/startpage, web/duckduckgo, web/ecosia, web/google, web/mojeek, web/public";

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
    WebHosted,
    WebEngine(String),
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

/// Validates provider-chain selectors only in the web role.
pub fn validate_role_selector(role: Role, text: &str) -> Result<(), AgentError> {
    let candidates = parse_selector(text)?;
    let invalid = candidates.iter().any(|candidate| {
        let web_candidate = matches!(
            candidate,
            SelectorCandidate::WebHosted | SelectorCandidate::WebEngine(_)
        );
        web_candidate != (role == Role::Web)
    });
    if invalid {
        return Err(AgentError::InvalidArgument(if role == Role::Web {
            "the web role accepts only web search selectors".into()
        } else {
            "web search selectors are only valid for the web role".into()
        }));
    }
    Ok(())
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
    if provider == "web" {
        if rest == "hosted" {
            return Ok(SelectorCandidate::WebHosted);
        }
        if roost_llm::web_search::ENGINE_IDS.contains(&rest) {
            return Ok(SelectorCandidate::WebEngine(rest.to_owned()));
        }
        return Err(AgentError::InvalidArgument(format!(
            "unknown web search engine {rest:?}"
        )));
    }
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
    let mut chain = Vec::new();
    expand_candidates(llm, settings, &candidates, &mut Vec::new(), &mut chain).await;
    Ok(chain.into_iter().next())
}

/// Resolves a role to its first available model, following the role's fallback.
pub async fn resolve_role(
    llm: &dyn Llm,
    settings: &AgentSettings,
    role: Role,
) -> Option<ResolvedModel> {
    role_chain(llm, settings, role).await.into_iter().next()
}

/// Every available model of a role in failover order. Quick, side and review
/// chains end with their parent role's models; Default and Advisor chains are
/// exclusive once configured. Empty when nothing resolves.
pub async fn role_chain(llm: &dyn Llm, settings: &AgentSettings, role: Role) -> Vec<ResolvedModel> {
    let mut chain = Vec::new();
    expand_role(llm, settings, role, &mut Vec::new(), &mut chain).await;
    chain
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
    let mut found = Vec::new();
    expand_candidates(
        llm,
        settings,
        &candidates,
        &mut vec![Role::Judge],
        &mut found,
    )
    .await;
    let mut chain: Vec<ModelInfo> = found.into_iter().map(|model| model.info).collect();
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

/// Appends the available models of `role` to `out`. `visited` breaks role
/// cycles (`@a` → `@b` → `@a`): a role already on the walk contributes nothing.
async fn expand_role(
    llm: &dyn Llm,
    settings: &AgentSettings,
    role: Role,
    visited: &mut Vec<Role>,
    out: &mut Vec<ResolvedModel>,
) {
    if visited.contains(&role) {
        return;
    }
    visited.push(role);
    if role == Role::Judge {
        for info in judge_candidates(llm, settings, None).await {
            push_unique(
                out,
                ResolvedModel {
                    info,
                    thinking: None,
                },
            );
        }
        return;
    }
    if let Some(configured) = settings.model_roles.get(&role)
        && let Ok(candidates) = parse_selector(configured)
    {
        let mut own = Vec::new();
        Box::pin(expand_candidates(
            llm,
            settings,
            &candidates,
            visited,
            &mut own,
        ))
        .await;
        if !own.is_empty() {
            for model in own {
                push_unique(out, model);
            }
            if !matches!(role, Role::Tiny | Role::Smol | Role::Slow) {
                return;
            }
        } else if role == Role::Advisor {
            // An explicitly configured advisor that cannot resolve stays off
            // rather than silently reviewing with a different model.
            return;
        }
    }
    match role {
        Role::Tiny => Box::pin(expand_role(llm, settings, Role::Smol, visited, out)).await,
        Role::Smol | Role::Slow => {
            Box::pin(expand_role(llm, settings, Role::Default, visited, out)).await
        }
        Role::Advisor => {
            if settings.model_roles.contains_key(&Role::Slow) {
                Box::pin(expand_role(llm, settings, Role::Slow, visited, out)).await;
            }
            Box::pin(expand_role(llm, settings, Role::Default, visited, out)).await;
        }
        Role::Default => {
            if let Some(found) = default_model(llm, settings).await {
                push_unique(out, found);
            }
        }
        Role::Plan | Role::Task | Role::Judge | Role::Web => {}
    }
}

/// Appends each candidate's available models to `out` in selector order.
async fn expand_candidates(
    llm: &dyn Llm,
    settings: &AgentSettings,
    candidates: &[SelectorCandidate],
    visited: &mut Vec<Role>,
    out: &mut Vec<ResolvedModel>,
) {
    for candidate in candidates {
        match candidate {
            SelectorCandidate::Model { .. } => {
                if let Some(found) = available_model(llm, candidate).await {
                    push_unique(out, found);
                }
            }
            SelectorCandidate::Role(role) => {
                Box::pin(expand_role(llm, settings, *role, visited, out)).await;
            }
            SelectorCandidate::WebHosted | SelectorCandidate::WebEngine(_) => {}
        }
    }
}

/// Appends `model` unless the chain already holds that model; the first pin wins.
fn push_unique(chain: &mut Vec<ResolvedModel>, model: ResolvedModel) {
    if !chain
        .iter()
        .any(|known| same_model(&known.info, &model.info))
    {
        chain.push(model);
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
