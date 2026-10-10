//! Ported from oh-my-pi packages/coding-agent/src/lsp/{tool,utils}.ts (MIT).
//! This file owns human-readable diagnostics, locations, hover and symbol output.
//! File names are rendered relative to the conversation working directory.

use std::path::Path;

use serde_json::Value;

use super::uri::uri_path;

pub(super) fn diagnostics_text(path: &Path, diagnostics: &[Value]) -> String {
    let lines = diagnostics
        .iter()
        .map(|diagnostic| format_diagnostic(path, diagnostic))
        .collect::<Vec<_>>();
    let errors = diagnostics
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_u64).unwrap_or(1) == 1)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|item| item.get("severity").and_then(Value::as_u64) == Some(2))
        .count();
    if lines.is_empty() {
        return "0 error(s), 0 warning(s)".into();
    }
    format!(
        "{}\n{errors} error(s), {warnings} warning(s)",
        lines.join("\n")
    )
}

pub(super) fn format_diagnostic(path: &Path, diagnostic: &Value) -> String {
    let line = diagnostic
        .pointer("/range/start/line")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let column = diagnostic
        .pointer("/range/start/character")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let severity = match diagnostic
        .get("severity")
        .and_then(Value::as_u64)
        .unwrap_or(1)
    {
        2 => "warning",
        3 => "info",
        4 => "hint",
        _ => "error",
    };
    let source = diagnostic
        .get("source")
        .and_then(Value::as_str)
        .map(|source| format!(" [{source}]"))
        .unwrap_or_default();
    let code = diagnostic
        .get("code")
        .map(|code| format!(" ({code})"))
        .unwrap_or_default();
    let message = diagnostic
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("diagnostic");
    format!(
        "{}:{line}:{column} [{severity}]{source} {message}{code}",
        path.display()
    )
}

pub(super) fn locations_text(cwd: &Path, result: &Value, references: bool) -> String {
    let locations = result.as_array().cloned().unwrap_or_else(|| {
        if result.is_null() {
            Vec::new()
        } else {
            vec![result.clone()]
        }
    });
    if locations.is_empty() {
        return "No locations found".into();
    }
    locations
        .iter()
        .take(if references { 50 } else { usize::MAX })
        .map(|location| {
            let uri = location
                .get("uri")
                .or_else(|| location.get("targetUri"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let position = location
                .pointer("/range/start")
                .or_else(|| location.pointer("/targetSelectionRange/start"))
                .or_else(|| location.pointer("/targetRange/start"));
            let line = position
                .and_then(|position| position.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let column = position
                .and_then(|position| position.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let path = uri_path(uri)
                .map(|path| relative(cwd, &path))
                .unwrap_or_else(|_| uri.to_owned());
            format!("{path}:{line}:{column}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn hover_text(result: &Value) -> String {
    let contents = &result["contents"];
    let rendered = match contents {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(markup_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(_) => markup_text(contents),
        _ => String::new(),
    };
    if rendered.is_empty() {
        "No hover information".into()
    } else {
        rendered
    }
}

pub(super) fn symbols_text(result: &Value) -> String {
    let Some(symbols) = result.as_array() else {
        return "No symbols found".into();
    };
    if symbols.is_empty() {
        return "No symbols found".into();
    }
    let mut output = Vec::new();
    for symbol in symbols {
        append_symbol(&mut output, symbol, 0);
    }
    output.join("\n")
}

pub(super) fn language_id(path: &Path) -> String {
    let base = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if base == "dockerfile" || base.starts_with("dockerfile.") || base == "containerfile" {
        return "dockerfile".into();
    }
    if base == "makefile" || base == "gnumakefile" {
        return "makefile".into();
    }
    if base == "justfile" {
        return "just".into();
    }
    if base == "cmakelists.txt" {
        return "cmake".into();
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "ts" | "cts" | "mts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "rs" => "rust",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hh" | "hpp" | "hxx" | "cu" | "cuh" | "ino" => "cpp",
        "zig" => "zig",
        "py" | "pyi" => "python",
        "rb" | "rbw" | "gemspec" => "ruby",
        "lua" => "lua",
        "sh" | "bash" | "zsh" | "ksh" | "bats" | "command" => "shellscript",
        "fish" => "fish",
        "pl" | "pm" | "perl" => "perl",
        "php" => "php",
        "java" => "java",
        "kt" | "ktm" | "kts" => "kotlin",
        "scala" | "sc" | "sbt" => "scala",
        "cs" => "csharp",
        "html" | "htm" | "xhtml" => "html",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "vue" => "vue",
        "svelte" => "svelte",
        "astro" => "astro",
        "json" => "json",
        "jsonc" => "jsonc",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "xsl" | "xslt" | "svg" | "plist" => "xml",
        "ini" => "ini",
        "md" | "markdown" | "mdx" | "mdc" | "mkd" | "mdown" => "markdown",
        "rst" => "restructuredtext",
        "adoc" => "asciidoc",
        "tex" => "latex",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "proto" => "protobuf",
        "tf" => "terraform",
        "hcl" => "hcl",
        "tfvars" => "hcl",
        "nix" => "nix",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "hs" => "haskell",
        "ml" | "mli" => "ocaml",
        "swift" => "swift",
        "dart" => "dart",
        "tla" | "tlaplus" => "tlaplus",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "bat",
        "cmake" => "cmake",
        _ => "plaintext",
    }
    .into()
}

fn markup_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn append_symbol(output: &mut Vec<String>, symbol: &Value, depth: usize) {
    let name = symbol
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>");
    let kind = symbol
        .get("kind")
        .and_then(Value::as_u64)
        .map(symbol_kind)
        .unwrap_or("symbol");
    let line = symbol
        .pointer("/range/start/line")
        .or_else(|| symbol.pointer("/location/range/start/line"))
        .and_then(Value::as_u64)
        .map(|line| line + 1);
    let suffix = line.map(|line| format!(" ({line})")).unwrap_or_default();
    output.push(format!("{}{kind} {name}{suffix}", "  ".repeat(depth)));
    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            append_symbol(output, child, depth + 1);
        }
    }
}

fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        23 => "Struct",
        26 => "TypeParameter",
        _ => "Symbol",
    }
}

fn relative(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::locations_text;

    #[test]
    fn definition_is_rendered_as_a_relative_path_and_position() {
        let cwd = std::env::temp_dir().join("lsp-render-test");
        let target = cwd.join("src/main.rs");
        let uri =
            url::Url::from_file_path(target).map_or_else(|_| String::new(), |url| url.to_string());
        let result = json!({"uri":uri,"range":{"start":{"line":4,"character":8},"end":{"line":4,"character":12}}});
        assert_eq!(locations_text(&cwd, &result, false), "src/main.rs:5:9");
    }
}
