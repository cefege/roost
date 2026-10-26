//! Tool path resolution (port of `packages/durable/src/tools/path-utils.ts`
//! @ `abe508e1b`).

use unicode_normalization::UnicodeNormalization;

use crate::chord::context::Context;
use crate::env::types::ExecutionEnv;
use crate::harness::types::ToolExecuteError;

/// Narrow no-break space used in the `AM`/`PM` read-path variant.
const NARROW_NO_BREAK_SPACE: char = '\u{202f}';

fn normalize_tool_path(path: &str) -> String {
    let normalized: String = path
        .chars()
        .map(|character| match character {
            '\u{00a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect();
    normalized
        .strip_prefix('@')
        .map(str::to_string)
        .unwrap_or(normalized)
}

/// Resolve a tool path against the environment's working directory.
pub async fn resolve_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &Context,
) -> Result<String, ToolExecuteError> {
    Ok(env
        .absolute_path(&normalize_tool_path(path), context)
        .await?)
}

/// Resolve a read-tool path, trying the common macOS/Unicode variants of the
/// resolved name.
pub async fn resolve_read_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &Context,
) -> Result<String, ToolExecuteError> {
    let resolved = resolve_tool_path(env, path, context).await?;
    let variants: Vec<String> = vec![
        resolved.clone(),
        // " (AM|PM)." uses a narrow no-break space in the variant.
        replace_ampm_space(&resolved),
        resolved.nfd().collect(),
        resolved.replace('\'', "\u{2019}"),
        resolved.nfd().collect::<String>().replace('\'', "\u{2019}"),
    ];
    let mut seen: Vec<String> = Vec::new();
    for variant in variants {
        if seen.contains(&variant) {
            continue;
        }
        seen.push(variant.clone());
        if env.exists(&variant, context).await? {
            return Ok(variant);
        }
    }
    Ok(resolved)
}

/// `resolved.replace(/ (AM|PM)\./gi, `${NARROW_NO_BREAK_SPACE}$1.`)`.
fn replace_ampm_space(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b' '
            && index + 4 <= bytes.len()
            && {
                let pair = &bytes[index + 1..index + 3];
                pair == b"AM" || pair == b"PM" || pair == b"am" || pair == b"pm"
            }
            && bytes[index + 3] == b'.'
        {
            result.push(NARROW_NO_BREAK_SPACE);
            result.push_str(&path[index + 1..index + 3]);
            result.push('.');
            index += 4;
            continue;
        }
        let character = path[index..].chars().next().unwrap_or_default();
        result.push(character);
        index += character.len_utf8();
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
    use std::os::unix::fs::PermissionsExt;

    fn context() -> Context {
        crate::chord::context::BACKGROUND_CONTEXT.clone()
    }

    /// A denied `exists` propagates like the upstream `getOrThrow` instead of
    /// being swallowed into a missing variant.
    #[tokio::test]
    async fn exists_errors_propagate_from_read_path_resolution() {
        let dir = std::env::temp_dir().join(format!("path-utils-{}", std::process::id()));
        let denied = dir.join("denied");
        std::fs::create_dir_all(&denied).expect("denied dir");
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        if std::fs::read_dir(&denied).is_ok() {
            // Root or a permissive file system: nothing to assert.
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).ok();
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let env = NodeExecutionEnv::new(NodeExecutionEnvOptions {
            cwd: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        });
        let error = resolve_read_tool_path(&env, "denied/x.txt", &context())
            .await
            .expect_err("a denied exists must propagate");
        assert!(!error.to_string().is_empty(), "got {error}");
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
