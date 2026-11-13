// Owns real builtin auth/account/reset interactions through the two source editors.
// Hooks supply only owned external HTTP, credentials seeds and lifecycle observations.
// Registry coverage uses exact commands/subcommands; behavior tags are not coverage IDs.
import type { BehaviorClaim, BuiltinCase, Checkpoint, Comparison } from "./scenario";
import { commandCase, effect, literalPattern, send } from "./builtin-steps";

const login = "command:login";
const logout = "command:logout";
const pin = ["command:session", "subcommand:session pin"];
const reset = ["command:usage", "subcommand:usage reset"];
const restart = "command:restart";
const ollama = "Ollama (Local OpenAI-compatible)";
const codex = "ChatGPT Plus/Pro (Codex Subscription)";
const prompt = '[data-omp-role="omp.login.prompt"] input';
const manualPrompt = '[data-omp-role="omp.login.prompt"] input';
const first = "parity-auth-a";
const second = "parity-auth-b";

function auth(pointer: string, expected: string | number | boolean | null): Comparison {
  return effect("builtin.auth", expected, pointer);
}
function provider(providerId: string, field: string, expected: string | number | boolean | null): Comparison {
  return auth(`/providers/${providerId}/${field}`, expected);
}
function http(pointer: string, expected: string | number | boolean | null): Comparison {
  return effect("builtin.auth-http", expected, pointer);
}
function account(accountId: string, field: string, expected: string | number | boolean | null): Comparison {
  return http(`/accounts/${accountId}/${field}`, expected);
}
function row(accountId: string, present: boolean): Comparison[] {
  return [provider("openai-codex", `accounts/${accountId}/present`, present), provider("openai-codex", `accounts/${accountId}/persisted`, present)];
}
function key(present: boolean): Comparison[] {
  return [provider("ollama", "apiKeyPresent", present), provider("ollama", "persistedApiKeyPresent", present), provider("ollama", "credentialCount", present ? 1 : 0), provider("ollama", "persistedCredentialCount", present ? 1 : 0)];
}
function ui(text: string): Comparison {
  const pattern = `(${literalPattern(text)})`;
  return { name: `Actual screen and native text: ${text}`, terminal: { type: "screen", pattern, group: 1 }, browser: { type: "dom", selector: `text=/${literalPattern(text)}/ >> nth=-1`, read: "text", pattern, group: 1 }, expected: text };
}
function visible(name: string, terminalPattern: string, selector: string): Comparison {
  return { name, terminal: { type: "screen", pattern: terminalPattern, read: "visible" }, browser: { type: "dom", selector, read: "visible" }, expected: true };
}
function checkpoint(text: string, comparisons: Comparison[] = []): Checkpoint {
  return { name: text, comparisons: [ui(text), ...comparisons] };
}
function claim(checkpoint: Checkpoint, ...entries: string[]): Checkpoint {
  return { ...checkpoint, comparisons: checkpoint.comparisons.map(comparison => ({ ...comparison, proves: entries.map((entry): BehaviorClaim => ({ entry, effect: comparison.terminal.type === "effect" ? "host" : "interaction", value: comparison.expected })) })) };
}
function workflow(id: string, command: string, opening: Checkpoint, steps: Checkpoint[], behaviors: string[]): BuiltinCase {
  const scenario = commandCase(id, command, opening.name, steps);
  scenario.checkpoints[0] = { ...opening, name: `1. ${opening.name}` };
  scenario.behaviors = behaviors;
  for (const step of scenario.checkpoints) {
    const comparisons = step.comparisons.map(comparison => ({ checkpoint: step.name, comparison: comparison.name }));
    for (const comparison of step.comparisons) {
      for (const proof of comparison.proves ?? []) proof.assertionGroups = [{ comparisons }];
    }
  }
  scenario.covers = [...new Set(scenario.checkpoints.flatMap(step => step.comparisons.flatMap(comparison => comparison.proves?.map(proof => proof.entry) ?? [])))];
  return scenario;
}
function escape(text: string, comparisons: Comparison[] = []): Checkpoint {
  return { ...checkpoint(text, comparisons), terminalInput: "\u001b", browserActions: [{ type: "press", key: "Escape" }] };
}
function restarted(count: number, comparisons: Comparison[]): Checkpoint {
  return send("/restart", "PARITY_AUTH_SEED_ANSWER", [effect("builtin.process", count, "/restartCount"), effect("builtin.session", true, "/sameSessionId"), effect("builtin.provider", 0, "/requestCount"), ...comparisons]);
}
function loggedIn(): Comparison[] {
  return [...row("parity-auth-login", true), provider("openai-codex", "credentialCount", 1), provider("openai-codex", "persistedCredentialCount", 1), provider("openai-codex", "accounts/parity-auth-login/accessPresent", true), provider("openai-codex", "accounts/parity-auth-login/refreshPresent", true), auth("/oauth/callbackListenerOpen", false), http("/tokenExchangeCount", 1), http("/tokenStatus", 200), http("/pkceVerified", true), http("/remoteRequests", 0), http("/rejectedRequests", 0)];
}
function oauthDialog(): Checkpoint {
  return {
    name: "Actual Codex authorization dialog before completion",
    comparisons: [visible("Canonical login dialog is mounted", literalPattern(`Login to ${codex}`), '[data-omp-role="omp.overlay.login"]'), ...row("parity-auth-login", false), auth("/oauth/callbackListenerOpen", true), auth("/oauth/authorizeStatePresent", true), auth("/oauth/authorizePkcePresent", true), http("/tokenExchangeCount", 0)],
  };
}
function pinned(accountId: string): Comparison[] {
  return [provider("openai-codex", "activeAccountId", accountId), provider("openai-codex", "stickyPresent", true), provider("openai-codex", "stickyExplicit", true), provider("openai-codex", "stickyMatchesSavedRow", true), ...row(first, true), ...row(second, true)];
}
function resetState(aCount: number, bCount: number, aConsume: number, bConsume: number): Comparison[] {
  return [account(first, "availableCount", aCount), account(second, "availableCount", bCount), account(first, "consumeCount", aConsume), account(second, "consumeCount", bConsume), http("/remoteRequests", 0), http("/rejectedRequests", 0)];
}
function pickerAccount(email: string, primary: string, comparisons: Comparison[]): Checkpoint {
  return { ...checkpoint(email, comparisons), terminalInput: "\r", browserActions: [{ type: "click", selector: `button:text-is(${JSON.stringify(primary)})` }] };
}

const apiKeyCase = workflow("auth-api-key-login-logout-restart", "/login", claim({
  name: "Canonical provider sign-in picker",
  comparisons: [visible("Actual provider selector", "Select provider to login", 'input[placeholder="Search providers…"]'), ...key(false)],
}, login), [
  claim({ ...checkpoint(ollama, key(false)), terminalInput: "Ollama", browserActions: [{ type: "fill", selector: 'input[placeholder="Search providers…"]', value: "Ollama" }] }, login),
  claim({ ...checkpoint("Paste your Ollama API key/token (optional)", key(false)), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Sign in")' }] }, login),
  claim({ ...checkpoint("Successfully logged in to ollama", key(true)), terminalInput: "parity-owned-ollama-key\r", browserActions: [{ type: "fill", selector: prompt, value: "parity-owned-ollama-key" }, { type: "click", selector: 'button:text-is("Continue")' }] }, login),
  claim(restarted(1, key(true)), login, restart),
  claim({ ...send("/logout", "Select provider"), comparisons: [visible("Actual provider sign-out selector", "Select provider to logout", 'input[placeholder="Search providers…"]'), ...key(true)] }, logout),
  claim({ ...checkpoint(ollama, key(true)), terminalInput: "Ollama", browserActions: [{ type: "fill", selector: 'input[placeholder="Search providers…"]', value: "Ollama" }] }, logout),
  claim({ ...checkpoint("API key", key(true)), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Sign out")' }] }, logout),
  claim({ ...checkpoint("Successfully logged out", key(false)), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Sign out")' }] }, logout),
  claim(restarted(2, key(false)), logout, restart),
  claim(send("/logout ollama", "Logout skipped: no stored credentials for ollama.", key(false)), logout),
  claim(send("/login ollama", "Paste your Ollama API key/token (optional)", key(false)), login),
  claim(escape("Login cancelled", key(false)), login),
  claim(send("/login manual", "No OAuth login is waiting for a manual callback.", key(false)), login),
], ["actual-provider-picker-login", "actual-api-key-prompt-store-remove", "owned-auth-presence-only", "login-cancel-no-mutation", "login-unpending-manual-argument-guard", "auth-restart-persistence"]);

function oauthSuccess(manual: boolean): BuiltinCase {
  const completion: Checkpoint = {
    ...checkpoint("Successfully logged in to openai-codex", loggedIn()),
    ...(manual ? { terminalInput: "\u001b[200~PARITY_AUTH_REDIRECT_URL\u001b[201~\r", browserActions: [{ type: "fill" as const, selector: manualPrompt, value: "PARITY_AUTH_REDIRECT_URL" }, { type: "click" as const, selector: 'button:text-is("Continue")' }] } : {}),
  };
  return workflow(`auth-oauth-${manual ? "manual" : "callback"}-restart`, "/login openai-codex", claim(oauthDialog(), login), [
    claim(completion, login),
    claim(restarted(1, loggedIn()), login, restart),
    claim(send("/logout openai-codex", "login@parity.invalid", [...row("parity-auth-login", true)]), logout),
    claim({ ...checkpoint("Successfully logged out login@parity.invalid", [...row("parity-auth-login", false), provider("openai-codex", "persistedCredentialCount", 0)]), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Sign out")' }] }, logout),
    claim(restarted(2, [...row("parity-auth-login", false), provider("openai-codex", "credentialCount", 0), auth("/oauth/callbackListenerOpen", false), http("/tokenExchangeCount", 1)]), logout, restart),
  ], [manual ? "actual-oauth-dialog-manual-redirect-paste" : "actual-oauth-http-callback", "actual-source-pkce-token-exchange", "actual-oauth-credential-write", "oauth-restart-persistence", "actual-oauth-row-logout"]);
}

const oauthCancel = workflow("auth-oauth-cancel", "/login openai-codex", claim(oauthDialog(), login), [
  claim(escape("Login cancelled", [...row("parity-auth-login", false), provider("openai-codex", "credentialCount", 0), provider("openai-codex", "persistedCredentialCount", 0), auth("/oauth/callbackListenerOpen", false), http("/tokenExchangeCount", 0), http("/remoteRequests", 0)]), login),
  claim(send("/login http://localhost:1455/auth/callback?code=unused&state=unused", "No OAuth login is waiting for a manual callback.", [...row("parity-auth-login", false), http("/tokenExchangeCount", 0)]), login),
], ["actual-oauth-cancel-aborts-listener", "cancel-removes-pending-manual-input", "oauth-cancel-does-not-write-credentials"]);

const oauthError = workflow("auth-oauth-exchange-error", "/login openai-codex", claim(oauthDialog(), login), [
  claim(checkpoint("PARITY_AUTH_EXCHANGE_REFUSED", [...row("parity-auth-login", false), provider("openai-codex", "credentialCount", 0), provider("openai-codex", "persistedCredentialCount", 0), auth("/oauth/callbackListenerOpen", false), http("/tokenExchangeCount", 1), http("/tokenStatus", 400), http("/pkceVerified", true), http("/remoteRequests", 0)]), login),
  claim(send("/logout openai-codex", "Logout skipped: no stored credentials for openai-codex.", [...row("parity-auth-login", false), http("/tokenExchangeCount", 1)]), logout),
], ["actual-oauth-token-exchange-error", "failed-exchange-does-not-write-row", "failed-exchange-retires-real-listener"]);

const accountPin = workflow("auth-account-pin-logout-restart", "/session pin", claim(checkpoint("a@parity.invalid", [...row(first, true), ...row(second, true), provider("openai-codex", "stickyExplicit", false)]), ...pin), [
  claim({ ...checkpoint("PARITY_AUTH_SEED_ANSWER", [...row(first, true), ...row(second, true), provider("openai-codex", "stickyExplicit", false)]), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }] }, ...pin),
  claim(send("/session pin", "a@parity.invalid", [...row(first, true), ...row(second, true)]), ...pin),
  claim({ ...checkpoint("Pinned b@parity.invalid to this session", pinned(second)), terminalInput: "\u001b[B\r", browserActions: [{ type: "click", selector: 'text="b@parity.invalid"' }, { type: "click", selector: 'button:text-is("Pin")' }] }, ...pin),
  claim(restarted(1, pinned(second)), ...pin, restart),
  claim(send("/session pin parity-missing", 'No ChatGPT Plus/Pro (Codex Subscription) account matches "parity-missing".', pinned(second)), ...pin),
  claim(send("/session pin a@parity.invalid", "Pinned a@parity.invalid to this session", pinned(first)), ...pin),
  claim(send("/session pin active", "Pinned a@parity.invalid to this session", pinned(first)), ...pin),
  claim(send("/session pin 2", "Pinned b@parity.invalid to this session", pinned(second)), ...pin),
  claim(send("/logout openai-codex", "b@parity.invalid", pinned(second)), logout),
  claim({ ...checkpoint("Successfully logged out b@parity.invalid", [...row(first, true), ...row(second, false), provider("openai-codex", "credentialCount", 1), provider("openai-codex", "stickyExplicit", false)]), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Sign out")' }] }, logout, ...pin),
  claim(restarted(2, [...row(first, true), ...row(second, false), provider("openai-codex", "persistedCredentialCount", 1), provider("openai-codex", "stickyExplicit", false)]), logout, ...pin, restart),
], ["actual-oauth-account-pin-selector", "pin-cancel-preserves-affinity", "pin-email-active-and-index-selectors", "pin-unmatched-error-preserves-pin", "explicit-pin-survives-restart", "actual-logout-reverses-pinned-account"]);

const usageSelection = workflow("auth-usage-reset-selection", "/usage reset", claim(checkpoint("a@parity.invalid", resetState(2, 1, 0, 0)), ...reset), [
  claim(pickerAccount("a@parity.invalid", "Spend reset", resetState(2, 1, 0, 0)), ...reset),
  claim({ ...checkpoint("a@parity.invalid", resetState(2, 1, 0, 0)), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Keep")' }] }, ...reset),
  claim({ ...checkpoint("PARITY_AUTH_SEED_ANSWER", resetState(2, 1, 0, 0)), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }] }, ...reset),
  claim(send("/usage reset", "a@parity.invalid", resetState(2, 1, 0, 0)), ...reset),
  claim({ ...checkpoint("b@parity.invalid", resetState(2, 1, 0, 0)), terminalInput: "\u001b[B\r", browserActions: [{ type: "click", selector: 'text="b@parity.invalid"' }, { type: "click", selector: 'button:text-is("Spend reset")' }] }, ...reset),
  claim({ ...checkpoint("Reset applied for b@parity.invalid (Codex)", [...resetState(2, 0, 0, 1), account(second, "consumedCreditId", "parity-auth-b-credit-1"), account(second, "lastConsumeStatus", 200), account(second, "lastConsumeCode", "reset")]), terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Spend")' }] }, ...reset),
  claim(send("/session pin a@parity.invalid", "Pinned a@parity.invalid to this session", pinned(first)), ...pin),
  claim(send("/usage reset codex/active", "Reset applied for a@parity.invalid (Codex)", [...resetState(1, 0, 1, 1), account(first, "consumedCreditId", "parity-auth-a-credit-1"), account(first, "lastConsumeStatus", 200)]), ...reset),
  claim(send("/usage reset absent/active", 'Unknown reset provider "absent". Use anthropic or openai-codex.', resetState(1, 0, 1, 1)), ...reset),
  claim(send("/usage show", "a@parity.invalid", [auth("/usageReportCount", 2), ...resetState(1, 0, 1, 1)]), "command:usage", "subcommand:usage show"),
  claim({ ...checkpoint("PARITY_AUTH_SEED_ANSWER", resetState(1, 0, 1, 1)), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }] }, "command:usage", "subcommand:usage show"),
  claim(restarted(1, [...resetState(1, 0, 1, 1), ...pinned(first)]), ...reset, restart),
  claim(send("/usage reset codex/active", "Reset applied for a@parity.invalid (Codex)", [...resetState(0, 0, 2, 1), account(first, "consumedCreditId", "parity-auth-a-credit-2")]), ...reset),
], ["actual-reset-account-selection", "actual-reset-double-confirm", "cancel-confirmation-does-not-consume", "actual-reset-nonactive-account-only", "actual-active-account-alias-reset", "reset-unknown-provider-guard", "actual-usage-dashboard", "reset-service-state-and-pin-survive-restart"]);

const usageRefused = workflow("auth-usage-reset-refused", "/session pin a@parity.invalid", claim(checkpoint("Pinned a@parity.invalid to this session", pinned(first)), ...pin), [
  claim(send("/usage reset codex/active", "a@parity.invalid (Codex): nothing to reset right now", [...resetState(2, 1, 1, 0), account(first, "lastConsumeCode", "nothing_to_reset"), account(first, "lastConsumeStatus", 200), account(first, "consumedCreditId", null)]), ...reset),
  claim(send("/usage reset", "a@parity.invalid", resetState(2, 1, 1, 0)), ...reset),
  claim({ ...checkpoint("PARITY_AUTH_SEED_ANSWER", resetState(2, 1, 1, 0)), terminalInput: "\u001b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }] }, ...reset),
], ["actual-reset-business-error", "refused-reset-keeps-credit-available", "error-reopens-real-reset-selector"]);

export const authWorkflowCases: BuiltinCase[] = [apiKeyCase, oauthSuccess(false), oauthSuccess(true), oauthCancel, oauthError, accountPin, usageSelection, usageRefused];
