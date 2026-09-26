//! Rendering a `ServiceSpec` as a macOS LaunchAgent plist. The Linux sibling
//! is systemd_unit.rs; both are called by definition_text.rs and neither is
//! allowed to invent a value the spec does not carry.
//!
//! A plist is XML, so every interpolated value is escaped — but the resource
//! ceilings the unit carries have no LaunchAgent equivalent and are simply not
//! rendered. launchd enforces a process limit and nothing else, which is a real
//! difference between the platforms rather than a gap in the template.

use roost_host::ProtocolResult;

use crate::services::service_spec::ServiceSpec;

/// The main log file a service's stdout is appended to.
const STDOUT_FILE: &str = "main.out.log";

/// The main log file a service's stderr is appended to.
const STDERR_FILE: &str = "main.err.log";

/// The plist header every Apple plist file starts with, including the DOCTYPE
/// launchd's own `plutil` expects before it will lint a file.
const PLIST_HEADER: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
    "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    "<plist version=\"1.0\">\n"
);

pub fn render_launchd_plist(spec: &ServiceSpec) -> ProtocolResult<String> {
    let mut plist = String::from(PLIST_HEADER);
    plist.push_str("<dict>\n");
    plist.push_str("  <key>Label</key>\n");
    plist.push_str(&string_entry(&spec.label));
    plist.push_str("  <key>ProgramArguments</key>\n");
    plist.push_str("  <array>\n");
    plist.push_str(&array_entry(&spec.program.display().to_string()));
    plist.push_str(&array_entry(spec.role.subcommand()));
    plist.push_str("  </array>\n");
    plist.push_str("  <key>WorkingDirectory</key>\n");
    plist.push_str(&string_entry(&spec.working_directory.display().to_string()));
    plist.push_str("  <key>EnvironmentVariables</key>\n");
    plist.push_str("  <dict>\n");
    for (key, value) in &spec.environment {
        plist.push_str(&format!("    <key>{}</key>\n", escape_xml_text(key)));
        plist.push_str(&format!(
            "    <string>{}</string>\n",
            escape_xml_text(value)
        ));
    }
    plist.push_str("  </dict>\n");
    plist.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    plist.push_str("  <key>KeepAlive</key>\n  <true/>\n");
    plist.push_str("  <key>ThrottleInterval</key>\n  <integer>1</integer>\n");
    plist.push_str("  <key>ProcessType</key>\n  <string>Interactive</string>\n");
    plist.push_str("  <key>StandardOutPath</key>\n");
    plist.push_str(&string_entry(&format!(
        "{}/{}",
        spec.log_dir.display(),
        STDOUT_FILE
    )));
    plist.push_str("  <key>StandardErrorPath</key>\n");
    plist.push_str(&string_entry(&format!(
        "{}/{}",
        spec.log_dir.display(),
        STDERR_FILE
    )));
    plist.push_str("</dict>\n</plist>\n");
    Ok(plist)
}

fn string_entry(value: &str) -> String {
    format!("  <string>{}</string>\n", escape_xml_text(value))
}

/// An entry inside `ProgramArguments`, one indent level deeper than a
/// top-level value.
fn array_entry(value: &str) -> String {
    format!("    <string>{}</string>\n", escape_xml_text(value))
}

/// The five characters XML reserves. A value is interpolated into a plist
/// exactly as an operator typed it, and a URL carrying `&` is ordinary, so
/// escaping is not optional here the way it is for a unit's raw path.
pub fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Whether `text` is a whole plist rather than a prefix of one. launchd will
/// not load an incomplete file and reports nothing for one it never loaded, so
/// a staged agent is proved with this before it is renamed into place.
pub fn plist_is_complete(text: &str) -> bool {
    if !text.starts_with("<?xml ") || !text.ends_with("</plist>\n") {
        return false;
    }
    let opened = text.matches("<dict>").count();
    let closed = text.matches("</dict>").count();
    opened == closed
        && text.contains("  <key>Label</key>\n")
        && text.contains("  <key>ProgramArguments</key>\n")
}

#[cfg(test)]
mod tests {
    use super::escape_xml_text;

    #[test]
    fn a_coordinator_url_keeps_its_ampersand() {
        assert_eq!(
            escape_xml_text("https://coord.example/?a=1&b=2"),
            "https://coord.example/?a=1&amp;b=2"
        );
    }

    #[test]
    fn a_quote_cannot_close_the_string_early() {
        assert_eq!(escape_xml_text(r#"a"b'c<d>e"#), "a&quot;b&apos;c&lt;d&gt;e");
    }
}
