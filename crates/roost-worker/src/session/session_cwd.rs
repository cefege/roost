//! The cwd a spawned or resumed session records: the requested folder with
//! `~` expanded to the user's home and symlinks resolved. Called by
//! `session::spawn`; depends on `roost_host`'s home lookup (`HOME`, else
//! `USERPROFILE`) so a Windows worker, which has no `HOME`, still expands `~`.

use roost_host::{EnvSource as _, ProcessEnv};

/// The spawn/resume cwd as the session record will report it.
///
/// realpath, not just tilde expansion: a symlinked request (`/tmp` on macOS is
/// `/private/tmp`) otherwise disagrees with the physical path the shell emits
/// over OSC 7 a moment later, and the SPA keys folders off that cwd — one
/// directory then splits into two folder groups. A missing directory keeps the
/// expanded value; the spawn fails later with the real error instead of having
/// it masked here.
pub fn canonical_session_cwd(value: &str, home: Option<&str>) -> String {
    let expanded = expand_home(value, home);
    match std::fs::canonicalize(&expanded) {
        Ok(resolved) => without_verbatim_prefix(resolved.to_string_lossy().into_owned()),
        Err(_) => expanded,
    }
}

fn expand_home(value: &str, home: Option<&str>) -> String {
    if value != "~" && !value.starts_with("~/") {
        return value.to_owned();
    }
    let Some(home) = home.map(str::to_owned).or_else(|| {
        ProcessEnv::new()
            .home_dir()
            .map(|home| home.to_string_lossy().into_owned())
    }) else {
        return value.to_owned();
    };
    if value == "~" {
        return home;
    }
    format!("{home}/{}", &value[2..])
}

#[cfg(unix)]
fn without_verbatim_prefix(path: String) -> String {
    path
}

/// `canonicalize` answers `\\?\C:\…` on Windows. The shell reports `C:\…` over
/// OSC 7, and cmd.exe refuses a `\\?\` working directory, so a drive path loses
/// the prefix; a `\\?\UNC\…` path keeps it, having no shorter exact form.
#[cfg(windows)]
fn without_verbatim_prefix(path: String) -> String {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => path,
    }
}
