//! Model catalog, connected accounts, settings and login state as the
//! coordinator returns them to clients over the agent RPCs.

use serde::{Deserialize, Serialize};

use super::model::ModelRef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsCatalog {
    pub models: Vec<ModelEntry>,
    pub providers: Vec<ProviderEntry>,
    pub thinking_levels: Vec<String>,
    pub default_model: Option<ModelRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub reasoning: bool,
    pub available: bool,
    /// A judgment (System One) model: offered only for the `judge` role.
    #[serde(default)]
    pub classifier: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub name: String,
    pub configured: bool,
    /// Every connected account of this provider, in sign-in order.
    pub accounts: Vec<AccountBrief>,
    pub supports_oauth: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBrief {
    pub credential_id: i64,
    pub label: String,
    pub kind: AccountKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Oauth,
    ApiKey,
}
/// One row of `AgentAccountsList` / `AgentUsageGet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountEntry {
    pub credential_id: i64,
    pub provider: String,
    pub label: String,
    pub kind: AccountKind,
    pub disabled_cause: Option<String>,
    pub blocked_until_ms: Option<i64>,
    pub usage: Option<AccountUsage>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUsage {
    pub windows: Vec<AccountUsageWindow>,
    pub note: Option<String>,
    pub fetched_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUsageWindow {
    pub name: String,
    pub used_fraction: f64,
    pub resets_at_ms: Option<i64>,
}
/// `AgentSettingsGet` / `AgentSettingsSet`: role → selector, keyed by role
/// name (`default`, `smol`, `slow`, `plan`, `task`, `tiny`, `judge`, `advisor`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettingsView {
    #[serde(default)]
    pub model_roles: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub default_model: Option<ModelRef>,
    #[serde(default)]
    pub advisor_enabled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub state: LoginStatus,
    pub prompt: Option<LoginPrompt>,
    pub notices: Vec<LoginNotice>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Prompt,
    Done,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub id: String,
    #[serde(rename = "type")]
    pub prompt_type: LoginPromptType,
    pub message: String,
    pub options: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginPromptType {
    Text,
    Secret,
    Select,
    ManualCode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginNotice {
    #[serde(rename = "type")]
    pub notice_type: LoginNoticeType,
    pub message: String,
    pub url: Option<String>,
    pub code: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginNoticeType {
    Info,
    AuthUrl,
    DeviceCode,
    Progress,
}
