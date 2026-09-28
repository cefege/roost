//! What an installed service definition on the TARGET says, for the two things
//! a deploy has to know before it touches it: which release the definition
//! currently runs, and which build that release was. Called by the build
//! identity proof and by settlement, which retires the prior release; the
//! `ROOST_*` environment it also reads comes from
//! [`crate::status::service_definition::parse_installed_environment`], so there
//! is one reader of an installed definition's environment in this crate.
//!
//! A deploy reads a definition written by an EARLIER release, which is why the
//! release directory is derived from the platform's own directive rather than
//! from this build's layout: the answer has to be the answer for the definition
//! in front of it.

use std::path::{Path, PathBuf};

use roost_host::HostPlatform;

use crate::status::service_definition::InstalledEnvironment;

/// The `WorkingDirectory` of an installed systemd unit, raw.
///
/// Raw, and never unquoted: `WorkingDirectory=` is not a quoted systemd
/// directive, and a value this module unquoted would be a value the unit never
/// had. The `%` doubling the writer applies is reversed here, because that is
/// the writer's own escape and the path on disk has a single `%` in it.
pub fn systemd_working_directory(definition: &str) -> Option<String> {
    let mut value: Option<&str> = None;
    for line in definition.lines() {
        if let Some(rest) = line.strip_prefix("WorkingDirectory=") {
            value = Some(rest.trim());
        }
    }
    value.map(|value| value.replace("%%", "%"))
}

/// The first program argument of an installed launchd agent, with the XML
/// entities the writer escaped resolved.
pub fn launchd_program_argument(definition: &str) -> Option<String> {
    let marker = "<key>ProgramArguments</key>";
    let array_start = definition.find(marker)?;
    let array = &definition[array_start..];
    let strings_start = array.find("<array>")? + "<array>".len();
    let strings_end = strings_start + array[strings_start..].find("</array>")?;
    let strings = &array[strings_start..strings_end];
    let open = strings.find("<string>")? + "<string>".len();
    let rest = &strings[open..];
    let close = rest.find("</string>")?;
    Some(unescape_xml(&rest[..close]))
}

/// The release directory an installed definition runs from, for either platform.
///
/// The two directives are not the same fact: systemd states a working directory
/// and a program, launchd states only the program. Both resolve to the release
/// root, which is the directory retirement is confined to.
pub fn installed_release_dir(definition: &str, platform: HostPlatform) -> Option<PathBuf> {
    match platform {
        HostPlatform::Linux => systemd_working_directory(definition).map(PathBuf::from),
        HostPlatform::MacOs => launchd_program_argument(definition)
            .and_then(|program| Path::new(&program).parent().map(Path::to_path_buf)),
        // v3 ships Linux and macOS only, and a deploy refuses a Windows target
        // before it reads a definition, so there is no third format to read.
        HostPlatform::Windows => None,
    }
}

/// The build the installed definition stamps, from either spelling.
///
/// A definition with no stamp is a source checkout that never had one, and that
/// is a distinct answer from a definition whose stamp is unreadable: the first
/// is allowed to be re-stamped, the second is not.
pub fn installed_build_sha(environment: &InstalledEnvironment) -> Option<String> {
    environment
        .get(roost_host::build_identity::ROOST_GIT_SHA_ENV)
        .or_else(|| environment.get(roost_host::build_identity::GIT_SHA_ENV))
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
}

fn unescape_xml(value: &str) -> String {
    // Ampersand last, exactly as the writer escaped it: resolving it first
    // would turn `&amp;lt;` into `<`.
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
