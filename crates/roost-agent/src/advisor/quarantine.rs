//! Ported from oh-my-pi packages/coding-agent/src/advisor/runtime.ts (MIT).
//! Rejects advisor output whose hazardous instruction is absent from reviewed
//! context, before that output can become an advisory card or model input.

use regex::Regex;

const HAZARDS: [(&str, &str); 3] = [
    (
        "account-deletion claim",
        r"(?i)\buser\b.{0,80}\b(?:deleted|erased)\b.{0,80}\baccount\b",
    ),
    (
        "instruction override",
        r"(?i)\bignore\s+(?:all\s+)?(?:prior|previous|earlier)\s+(?:user\s+)?instructions\b",
    ),
    (
        "denial instruction",
        r"(?i)\bdeny\s+(?:this|it|the\s+request)\s+if\s+(?:asked|questioned)\b",
    ),
];

pub(crate) fn unsafe_output(generated: &str, source: &str) -> Option<String> {
    let mut output_only = Vec::new();
    for (label, pattern) in HAZARDS {
        let Ok(regex) = Regex::new(pattern) else {
            continue;
        };
        if regex.is_match(generated) && !regex.is_match(source) {
            output_only.push(label);
        }
    }
    let generated_destructive = has_destructive_shell_command(generated);
    let source_destructive = has_destructive_shell_command(source);
    if generated_destructive && !source_destructive {
        output_only.push("destructive shell command");
    }
    let generated_override = HAZARDS.iter().any(|(label, pattern)| {
        *label == "instruction override"
            && Regex::new(pattern).is_ok_and(|regex| regex.is_match(generated))
    });
    if generated_destructive
        && generated_override
        && output_only.contains(&"instruction override")
        && !output_only.contains(&"destructive shell command")
    {
        output_only.push("destructive shell command");
    }
    if output_only.contains(&"destructive shell command") || output_only.len() >= 3 {
        Some(format!(
            "Advisor response quarantined: generated output-only directives: {}",
            output_only.join(", ")
        ))
    } else {
        None
    }
}

fn has_destructive_shell_command(text: &str) -> bool {
    let Ok(regex) = Regex::new(r"(?i)\brm\s+(?P<flags>(?:-[a-z]+\s*)+)") else {
        return false;
    };
    regex.captures_iter(text).any(|capture| {
        let flags = capture
            .name("flags")
            .map(|value| value.as_str())
            .unwrap_or_default();
        let has_recursive = flags
            .split_whitespace()
            .any(|flag| flag.starts_with('-') && flag[1..].to_ascii_lowercase().contains('r'));
        let has_force = flags
            .split_whitespace()
            .any(|flag| flag.starts_with('-') && flag[1..].to_ascii_lowercase().contains('f'));
        has_recursive && has_force
    })
}
