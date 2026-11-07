//! Port of `packages/agent/src/harness/system-prompt.ts` @ pi 0.82.1
//! (2efa728) — `formatSkillsForSystemPrompt`.
//!
//! This is the harness copy of the system-prompt helper; the coding-agent
//! copy (`crate::rpi::core::system_prompt` in the `rpi` crate) gates the
//! skill list on the read tool being active (`tools.includes("read")` /
//! `selectedTools.includes("read")`). The harness version has **no such
//! gate** (system-prompt.ts:4) and must not gain one.
//!
//! Intentional differences: none beyond the Rust spelling.

use crate::harness::types::Skill;

/// `formatSkillsForSystemPrompt` (system-prompt.ts:3-25) — render the
/// model-visible skill list as an `<available_skills>` XML block. Skills
/// with `disableModelInvocation` are skipped; the remaining skills keep
/// their input order (upstream does not sort). Returns `""` when no skill
/// is model-visible.
pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    let visible_skills: Vec<&Skill> = skills
        .iter()
        .filter(|skill| skill.disable_model_invocation != Some(true))
        .collect();
    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Read the full skill file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands."
            .to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// `escapeXml` (system-prompt.ts:27-34) — escape the five XML entities in
/// order; the later passes never touch entities introduced by earlier ones.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(
        name: &str,
        description: &str,
        content: &str,
        file_path: &str,
        disable_model_invocation: Option<bool>,
    ) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            file_path: file_path.to_string(),
            disable_model_invocation,
        }
    }

    /// Upstream `formats visible skills in order and skips model-disabled
    /// skills` (system-prompt.test.ts:27).
    #[test]
    fn test_formats_visible_skills_in_order_skips_disabled() {
        let visible = skill(
            "visible",
            "Use <this> & that",
            "visible content",
            "/skills/visible/SKILL.md",
            None,
        );
        let second = skill(
            "second",
            "Second skill",
            "second content",
            "/skills/second/SKILL.md",
            None,
        );
        let disabled = skill(
            "hidden",
            "Hidden",
            "hidden content",
            "/skills/hidden/SKILL.md",
            Some(true),
        );

        assert_eq!(
            format_skills_for_system_prompt(&[visible, disabled, second]),
            "The following skills provide specialized instructions for specific tasks.
Read the full skill file when the task matches its description.
When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.

<available_skills>
  <skill>
    <name>visible</name>
    <description>Use &lt;this&gt; &amp; that</description>
    <location>/skills/visible/SKILL.md</location>
  </skill>
  <skill>
    <name>second</name>
    <description>Second skill</description>
    <location>/skills/second/SKILL.md</location>
  </skill>
</available_skills>"
        );
    }

    /// Upstream `returns an empty string when no skills are model-visible`
    /// (system-prompt.test.ts:48). A `Some(false)` skill is visible too.
    #[test]
    fn test_returns_empty_string_when_none_visible() {
        let disabled = skill(
            "hidden",
            "Hidden",
            "hidden content",
            "/s/SKILL.md",
            Some(true),
        );
        assert_eq!(format_skills_for_system_prompt(&[disabled]), "");

        let visible = skill("ok", "Ok", "content", "/s/SKILL.md", Some(false));
        assert!(format_skills_for_system_prompt(&[visible]).contains("<name>ok</name>"));
    }

    /// Upstream `escapes XML in all model-visible skill fields`
    /// (system-prompt.test.ts:52).
    #[test]
    fn test_escapes_xml_in_all_fields() {
        let skill = skill(
            "a&b",
            "Quote \"double\" and 'single'",
            "content",
            "/skills/<bad>&\"quote\"/SKILL.md",
            None,
        );
        assert!(
            format_skills_for_system_prompt(&[skill]).contains(
                "<name>a&amp;b</name>\n    <description>Quote &quot;double&quot; and &apos;single&apos;</description>\n    <location>/skills/&lt;bad&gt;&amp;&quot;quote&quot;/SKILL.md</location>"
            )
        );
    }
}
