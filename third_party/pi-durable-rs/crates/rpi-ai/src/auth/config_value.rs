//! Port of `packages/coding-agent/src/core/resolve-config-value.ts`
//! @ pi 0.82.1 (2efa728).
//!
//! Resolve configuration values that may be shell commands, environment
//! variables, or literals. Used by `file_store.rs` (auth.json) and, with
//! T09, models.json.
//!
//! Intentional differences:
//! - The win32 configured-shell branch (`executeWithConfiguredShell`,
//!   `getShellConfig`) is not ported: rpi targets unix shells first and the
//!   upstream default `execSync` shell on unix is `/bin/sh -c`, which is what
//!   [`execute_command_uncached`] always uses here.
//! - Failures surface as [`ModelsError`] (code `auth`) instead of `Error`.
//! - `resolve_headers` operates on [`ProviderHeaders`] whose `None` values
//!   are header-suppression markers; they pass through unresolved (upstream
//!   `Record<string, string>` has no such marker).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::resolve::{ModelsError, ModelsErrorCode};
use crate::types::{ProviderEnv, ProviderHeaders};
use crate::utils::provider_env::get_provider_env_value;

/// Upstream `timeout: 10000` (ms) for command config values.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(10_000);

/// `commandResultCache` — command results persist for the process lifetime.
fn command_result_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `ENV_VAR_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/`.
fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.bytes();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == b'_' => {}
        _ => return false,
    }
    chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `ENV_VAR_NAME_PREFIX_RE = /^[A-Za-z_][A-Za-z0-9_]*/` — match length.
fn env_var_name_prefix_len(s: &str) -> usize {
    let mut len = 0;
    for (i, b) in s.bytes().enumerate() {
        let ok = if i == 0 {
            b.is_ascii_alphabetic() || b == b'_'
        } else {
            b.is_ascii_alphanumeric() || b == b'_'
        };
        if !ok {
            break;
        }
        len += 1;
    }
    len
}

/// `TemplatePart`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

/// `ConfigValueReference`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

/// `appendLiteral`: adjacent literal runs merge into one part.
fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_owned()));
}

/// `parseConfigValueTemplate`. All delimiters (`$`, `{`, `}`) are ASCII, so
/// byte indexing matches upstream's UTF-16 code-unit indexing.
fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts = Vec::new();
    let mut index = 0;

    while index < config.len() {
        let Some(dollar_index) = config[index..].find('$').map(|i| index + i) else {
            append_literal(&mut parts, &config[index..]);
            break;
        };

        append_literal(&mut parts, &config[index..dollar_index]);
        let next_char = config.as_bytes().get(dollar_index + 1).copied();

        // "$$" escapes a literal "$", "$!" escapes a literal "!".
        if let Some(b'$') | Some(b'!') = next_char {
            append_literal(&mut parts, &config[dollar_index + 1..dollar_index + 2]);
            index = dollar_index + 2;
            continue;
        }

        if next_char == Some(b'{') {
            match config[dollar_index + 2..].find('}') {
                None => {
                    append_literal(&mut parts, "$");
                    index = dollar_index + 1;
                    continue;
                }
                Some(rel) => {
                    let end_index = dollar_index + 2 + rel;
                    let name = &config[dollar_index + 2..end_index];
                    if is_env_var_name(name) {
                        parts.push(TemplatePart::Env(name.to_owned()));
                    } else {
                        append_literal(&mut parts, &config[dollar_index..=end_index]);
                    }
                    index = end_index + 1;
                    continue;
                }
            }
        }

        let rest = &config[dollar_index + 1..];
        let prefix_len = env_var_name_prefix_len(rest);
        if prefix_len > 0 {
            parts.push(TemplatePart::Env(rest[..prefix_len].to_owned()));
            index = dollar_index + 1 + prefix_len;
            continue;
        }

        // Bare "$" (or "$" before a non-name char): literal.
        append_literal(&mut parts, "$");
        index = dollar_index + 1;
    }

    parts
}

/// `parseConfigValueReference`: a leading `!` makes the whole string a
/// command; anything else is a template.
fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if let Some(command) = config.strip_prefix('!') {
        return ConfigValueReference::Command(command.to_owned());
    }
    ConfigValueReference::Template(parse_config_value_template(config))
}

/// `resolveEnvConfigValue` — `env?.[name] || process.env[name] || undefined`.
/// [`get_provider_env_value`] already mirrors the JS `||` chain (empty
/// strings fall through).
fn resolve_env_config_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    get_provider_env_value(name, env)
}

/// `getTemplateEnvVarNames` — first-seen order, no duplicates.
fn get_template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

/// `resolveTemplate`: any missing env var resolves the whole value to `None`.
fn resolve_template(parts: &[TemplatePart], env: Option<&ProviderEnv>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => {
                resolved.push_str(&resolve_env_config_value(name, env)?);
            }
        }
    }
    Some(resolved)
}

/// `getConfigValueEnvVarName` — single-env-var templates only.
pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    let ConfigValueReference::Template(parts) = parse_config_value_reference(config) else {
        return None;
    };
    match parts.as_slice() {
        [TemplatePart::Env(name)] => Some(name.clone()),
        _ => None,
    }
}

/// `getConfigValueEnvVarNames`.
pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => get_template_env_var_names(&parts),
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

/// `getMissingConfigValueEnvVarNames`.
pub fn get_missing_config_value_env_var_names(
    config: &str,
    env: Option<&ProviderEnv>,
) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

/// `isCommandConfigValue`.
pub fn is_command_config_value(config: &str) -> bool {
    matches!(
        parse_config_value_reference(config),
        ConfigValueReference::Command(_)
    )
}

/// `isConfigValueConfigured`.
pub fn is_config_value_configured(config: &str, env: Option<&ProviderEnv>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

/// `resolveConfigValue`:
/// - leading `!` executes the rest as a shell command and uses stdout (cached)
/// - `$ENV_VAR` / `${ENV_VAR}` interpolate the named environment variable
/// - in non-command values, `$$` escapes a literal `$` and `$!` a literal `!`
/// - anything else is a literal
pub fn resolve_config_value(config: &str, env: Option<&ProviderEnv>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => execute_command(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// `resolveConfigValueUncached` — commands execute on every call.
pub fn resolve_config_value_uncached(config: &str, env: Option<&ProviderEnv>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => execute_command_uncached(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// `resolveConfigValueOrThrow` — error messages mirror upstream verbatim.
pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&ProviderEnv>,
) -> Result<String, ModelsError> {
    if let Some(resolved) = resolve_config_value_uncached(config, env) {
        return Ok(resolved);
    }

    let reference = parse_config_value_reference(config);
    if let ConfigValueReference::Command(command) = &reference {
        return Err(ModelsError::new(
            ModelsErrorCode::Auth,
            format!("Failed to resolve {description} from shell command: {command}"),
        ));
    }

    if matches!(reference, ConfigValueReference::Template(_)) {
        let missing = get_missing_config_value_env_var_names(config, env);
        if missing.len() == 1 {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!(
                    "Failed to resolve {description} from environment variable: {}",
                    missing[0]
                ),
            ));
        }
        if missing.len() > 1 {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!(
                    "Failed to resolve {description} from environment variables: {}",
                    missing.join(", ")
                ),
            ));
        }
    }

    Err(ModelsError::new(
        ModelsErrorCode::Auth,
        format!("Failed to resolve {description}"),
    ))
}

/// `executeCommandUncached` — unix path only (`/bin/sh -c`, the upstream
/// `execSync` default shell); the win32 configured-shell branch is not
/// ported (see module docs). Stdin closed, stderr discarded, stdout trimmed;
/// empty output, non-zero exit, spawn error and timeout all resolve to `None`.
fn execute_command_uncached(command: &str) -> Option<String> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Read stdout on a helper thread so a chatty command cannot deadlock on
    // a full pipe while we poll for exit.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= COMMAND_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };

    let output = reader.join().ok()?;
    if !status?.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output).trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// `executeCommand` — process-lifetime cache of both hits and misses.
fn execute_command(command: &str) -> Option<String> {
    {
        let cache = command_result_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(command) {
            return cached.clone();
        }
    }
    let result = execute_command_uncached(command);
    command_result_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(command.to_owned(), result.clone());
    result
}

/// `resolveHeaders` — resolve all header values with the same logic as API
/// keys. Unresolvable values are dropped; `None` values (header-suppression
/// markers, see module docs) pass through. `None` when nothing resolves.
pub fn resolve_headers(
    headers: Option<&ProviderHeaders>,
    env: Option<&ProviderEnv>,
) -> Option<ProviderHeaders> {
    let headers = headers?;
    let mut resolved = ProviderHeaders::new();
    for (key, value) in headers {
        match value {
            Some(value) => {
                if let Some(resolved_value) =
                    resolve_config_value(value, env).filter(|v| !v.is_empty())
                {
                    resolved.insert(key.clone(), Some(resolved_value));
                }
            }
            None => {
                resolved.insert(key.clone(), None);
            }
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// `resolveHeadersOrThrow`.
pub fn resolve_headers_or_throw(
    headers: Option<&ProviderHeaders>,
    description: &str,
    env: Option<&ProviderEnv>,
) -> Result<Option<ProviderHeaders>, ModelsError> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved = ProviderHeaders::new();
    for (key, value) in headers {
        match value {
            Some(value) => {
                let resolved_value = resolve_config_value_or_throw(
                    value,
                    &format!("{description} header \"{key}\""),
                    env,
                )?;
                resolved.insert(key.clone(), Some(resolved_value));
            }
            None => {
                resolved.insert(key.clone(), None);
            }
        }
    }
    if resolved.is_empty() {
        Ok(None)
    } else {
        Ok(Some(resolved))
    }
}

/// `clearConfigValueCache` — exported for testing.
pub fn clear_config_value_cache() {
    command_result_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
mod tests {
    //! Test intents ported from
    //! `packages/coding-agent/test/resolve-config-value.test.ts`
    //! @ pi 0.82.1 (2efa728), same names in snake_case.
    //!
    //! Not ported: "uses stdin when the configured Windows shell requires it"
    //! exercises the win32 configured-shell branch, which is not ported (see
    //! module docs).

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "rpi-config-value-{}-{id}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    struct EnvGuard(&'static str);

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            // Distinct variable names per test: process env is global, so
            // tests never share a name (upstream does the same).
            rpi_test_env::set_var(name, value);
            Self(name)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            rpi_test_env::remove_var(self.0);
        }
    }

    #[test]
    fn resolves_literals_environment_templates_and_escapes() {
        let _left = EnvGuard::set("TEST_CONFIG_LEFT", "left");
        let _right = EnvGuard::set("TEST_CONFIG_RIGHT", "right");

        assert_eq!(
            resolve_config_value("literal-key", None).as_deref(),
            Some("literal-key")
        );
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_LEFT", None).as_deref(),
            Some("left")
        );
        assert_eq!(
            resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", None).as_deref(),
            Some("left_right")
        );
        assert_eq!(
            resolve_config_value("$$TEST_CONFIG_LEFT", None).as_deref(),
            Some("$TEST_CONFIG_LEFT")
        );
        assert_eq!(
            resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", None).as_deref(),
            Some("!literal-right")
        );
    }

    #[test]
    fn uses_credential_scoped_environment_before_process_env() {
        let _scoped = EnvGuard::set("TEST_CONFIG_SCOPED", "process");
        let env = ProviderEnv::from([("TEST_CONFIG_SCOPED".to_owned(), "credential".to_owned())]);
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_SCOPED", Some(&env)).as_deref(),
            Some("credential")
        );
    }

    #[test]
    fn executes_shell_commands_and_trims_their_output() {
        assert_eq!(
            resolve_config_value("!echo '  spaced-key  '", None).as_deref(),
            Some("spaced-key")
        );
        assert_eq!(
            resolve_config_value("!printf 'line1\\nline2'", None).as_deref(),
            Some("line1\nline2")
        );
        assert_eq!(
            resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
            Some("hello-world")
        );
    }

    #[test]
    fn returns_undefined_when_command_resolution_fails() {
        // These commands are only resolved through the uncached path so the
        // process-lifetime cache cannot leak failures into other tests.
        for command in ["!exit 1", "!nonexistent-command-12345", "!printf ''"] {
            assert_eq!(resolve_config_value_uncached(command, None), None);
        }
    }

    #[test]
    fn caches_successful_and_failed_commands_until_explicitly_cleared() {
        // Only this test touches the cache lifecycle (upstream clears in
        // beforeEach/afterEach); other tests use distinct command strings.
        clear_config_value_cache();
        let dir = temp_dir("counter");
        let counter_file = dir.join("counter");
        std::fs::write(&counter_file, "0").expect("seed counter");
        let path = counter_file.display();

        let success = format!(
            "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
        );
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            std::fs::read_to_string(&counter_file).expect("read").trim(),
            "1"
        );

        clear_config_value_cache();
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            std::fs::read_to_string(&counter_file).expect("read").trim(),
            "2"
        );

        let failure =
            format!("!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; exit 1'");
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(
            std::fs::read_to_string(&counter_file).expect("read").trim(),
            "3"
        );

        clear_config_value_cache();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_cache_environment_values() {
        let dynamic: &'static str = "TEST_CONFIG_DYNAMIC";
        rpi_test_env::set_var(dynamic, "first");
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
            Some("first")
        );
        rpi_test_env::set_var(dynamic, "second");
        assert_eq!(
            resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
            Some("second")
        );
        rpi_test_env::remove_var(dynamic);
    }

    #[test]
    fn uncached_resolution_executes_a_command_on_every_call() {
        let dir = temp_dir("uncached-counter");
        let counter_file = dir.join("uncached-counter");
        std::fs::write(&counter_file, "0").expect("seed counter");
        let path = counter_file.display();
        let command = format!(
            "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
        );
        assert_eq!(
            resolve_config_value_uncached(&command, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value_uncached(&command, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            std::fs::read_to_string(&counter_file).expect("read").trim(),
            "2"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // Direct coverage for the remaining exported surface (env-var name
    // helpers, or-throw messages, headers) — no upstream test counterpart.
    // ------------------------------------------------------------------

    #[test]
    fn env_var_name_helpers_match_template_shapes() {
        assert_eq!(
            get_config_value_env_var_name("$FOO"),
            Some("FOO".to_owned())
        );
        assert_eq!(
            get_config_value_env_var_name("${FOO}"),
            Some("FOO".to_owned())
        );
        assert_eq!(get_config_value_env_var_name("prefix-$FOO"), None);
        assert_eq!(get_config_value_env_var_name("!echo hi"), None);
        assert_eq!(get_config_value_env_var_name("literal"), None);

        assert_eq!(
            get_config_value_env_var_names("${A}_$B-$A"),
            vec!["A".to_owned(), "B".to_owned()]
        );
        assert!(get_config_value_env_var_names("!echo $A").is_empty());
        assert!(get_config_value_env_var_names("$$A").is_empty());
    }

    #[test]
    fn missing_env_var_names_and_configured_checks() {
        let _set = EnvGuard::set("TEST_CONFIG_PRESENT", "yes");
        let env = ProviderEnv::from([("TEST_CONFIG_SCOPED_ONLY".to_owned(), "v".to_owned())]);

        assert_eq!(
            get_missing_config_value_env_var_names(
                "$TEST_CONFIG_PRESENT-$TEST_CONFIG_MISSING_A-${TEST_CONFIG_MISSING_B}",
                None
            ),
            vec![
                "TEST_CONFIG_MISSING_A".to_owned(),
                "TEST_CONFIG_MISSING_B".to_owned()
            ]
        );
        assert!(is_config_value_configured("$TEST_CONFIG_PRESENT", None));
        assert!(!is_config_value_configured("$TEST_CONFIG_MISSING_A", None));
        // Scoped env counts toward configured-ness (upstream: same helper).
        assert!(is_config_value_configured(
            "$TEST_CONFIG_SCOPED_ONLY",
            Some(&env)
        ));
        // Empty-string scoped values are falsy upstream (`||` chain).
        let empty = ProviderEnv::from([("TEST_CONFIG_PRESENT".to_owned(), String::new())]);
        assert!(is_config_value_configured(
            "$TEST_CONFIG_PRESENT",
            Some(&empty)
        ));
        assert!(is_command_config_value("!echo hi"));
        assert!(!is_command_config_value("$!echo"));
    }

    #[test]
    fn or_throw_messages_mirror_upstream() {
        let error = resolve_config_value_or_throw("!exit 1", "API key", None).expect_err("fails");
        assert_eq!(
            error.message,
            "Failed to resolve API key from shell command: exit 1"
        );

        let error = resolve_config_value_or_throw("$TEST_CONFIG_OR_THROW_MISSING", "API key", None)
            .expect_err("fails");
        assert_eq!(
            error.message,
            "Failed to resolve API key from environment variable: TEST_CONFIG_OR_THROW_MISSING"
        );

        let error = resolve_config_value_or_throw(
            "$TEST_CONFIG_OR_THROW_A-$TEST_CONFIG_OR_THROW_B",
            "API key",
            None,
        )
        .expect_err("fails");
        assert_eq!(
            error.message,
            "Failed to resolve API key from environment variables: TEST_CONFIG_OR_THROW_A, TEST_CONFIG_OR_THROW_B"
        );

        // Empty template: upstream `resolvedValue !== undefined` ("" is not
        // undefined), so it resolves to the empty string; the bare
        // `Failed to resolve ${description}` fallback is unreachable
        // upstream (kept for shape parity).
        let resolved = resolve_config_value_or_throw("", "API key", None).expect("empty resolves");
        assert_eq!(resolved, "");
    }

    #[test]
    fn resolve_headers_drops_unresolvable_and_keeps_suppression_markers() {
        let _set = EnvGuard::set("TEST_CONFIG_HEADER", "header-value");
        let headers = ProviderHeaders::from([
            ("X-Static".to_owned(), Some("static".to_owned())),
            ("X-Env".to_owned(), Some("$TEST_CONFIG_HEADER".to_owned())),
            (
                "X-Missing".to_owned(),
                Some("$TEST_CONFIG_HEADER_MISSING".to_owned()),
            ),
            ("X-Suppress".to_owned(), None),
        ]);
        let resolved = resolve_headers(Some(&headers), None).expect("some resolve");
        assert_eq!(resolved.get("X-Static"), Some(&Some("static".to_owned())));
        assert_eq!(
            resolved.get("X-Env"),
            Some(&Some("header-value".to_owned()))
        );
        assert!(!resolved.contains_key("X-Missing"));
        assert_eq!(resolved.get("X-Suppress"), Some(&None));

        assert_eq!(resolve_headers(None, None), None);
        let empty = ProviderHeaders::from([(
            "X-Missing".to_owned(),
            Some("$TEST_CONFIG_HEADER_MISSING".to_owned()),
        )]);
        assert_eq!(resolve_headers(Some(&empty), None), None);
    }

    #[test]
    fn resolve_headers_or_throw_names_the_header() {
        let headers = ProviderHeaders::from([(
            "X-Key".to_owned(),
            Some("$TEST_CONFIG_HEADER_THROW_MISSING".to_owned()),
        )]);
        let error = resolve_headers_or_throw(Some(&headers), "Anthropic", None)
            .expect_err("missing env var");
        assert_eq!(
            error.message,
            "Failed to resolve Anthropic header \"X-Key\" from environment variable: TEST_CONFIG_HEADER_THROW_MISSING"
        );
        assert_eq!(
            resolve_headers_or_throw(None, "Anthropic", None).expect("ok"),
            None
        );
    }
}
