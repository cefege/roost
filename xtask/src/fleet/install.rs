//! `fleet install`: put one built tag on a host and restart its services. The
//! release lands in its own `versions/<tag>/` directory beside the running one,
//! the service definitions are repointed at it, and the restart must leave the
//! keeper process — which owns every PTY — exactly where it was.

use std::path::PathBuf;
use std::time::Instant;

use super::manifest::{FleetHost, Platform};

const STAGE_DIR: &str = ".roost-stage";
/// How long every restarted service must keep one main pid. A worker that
/// cannot admit its keeper exits after the 5 s identity deadline and systemd
/// or launchd starts it again, so a shorter window passes a crash loop.
const STABLE_SECONDS: u32 = 12;
/// Prints the keeper pid, or `dead:<pid file contents>` when no such process
/// runs; a pid file outlives a reboot. Expects `$ROOT`.
const KEEPER_PID_LINE: &str = "PID=$(cat \"$ROOT/mux-keeper.pid\" 2>/dev/null || echo none)\n\
if [ \"$PID\" != none ] && kill -0 \"$PID\" 2>/dev/null; then echo \"$PID\"; else echo \"dead:$PID\"; fi\n";
const COORDINATOR_URL: &str = "https://mike.roosttt.com";

/// A tag `fleet build` finished: its directory and the commit it was built at.
pub struct BuiltRelease {
    tag: String,
    sha: String,
    dir: PathBuf,
}

impl BuiltRelease {
    pub fn load(tag: &str) -> Result<Self, String> {
        let dir = super::release_dir(tag);
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}; run fleet build first", path.display()))?;
        let manifest: serde_json::Value =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let field = |name: &str| {
            manifest[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} has no {name}", path.display()))
        };
        if field("version")? != tag {
            return Err(format!("{} describes another tag", path.display()));
        }
        let sha = field("sha")?;
        if sha.len() < 8 || !sha.chars().all(|character| character.is_ascii_hexdigit()) {
            return Err(format!("{} carries a malformed sha", path.display()));
        }
        Ok(Self {
            tag: tag.to_owned(),
            sha,
            dir,
        })
    }

    /// The release tag.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The commit the release was built at.
    pub fn sha(&self) -> &str {
        &self.sha
    }
}

pub fn install_on(host: &FleetHost, release: &BuiltRelease) -> Result<(), String> {
    let started = Instant::now();
    let tag = &release.tag;
    let stage = format!("{STAGE_DIR}/{tag}");
    host.unpack_into_home(
        &release.dir.join(host.platform.artifact_dir()),
        &format!("{stage}/bin"),
    )?;
    host.unpack_into_home(&release.dir.join("web"), &format!("{stage}/web"))?;

    let placed = host.run_script("place the release", &place_script(host.platform, tag))?;
    let mut lines = placed.lines().map(str::trim);
    let version = lines.next().unwrap_or_default();
    let build_sha = lines.next().unwrap_or_default();
    let keeper_before = lines.next().unwrap_or_default().to_owned();
    if version != tag || !build_sha.starts_with(&release.sha[..8]) {
        return Err(format!(
            "{}: the placed binary reports {version} {build_sha}, not {tag} {}",
            host.name,
            &release.sha[..8]
        ));
    }

    let restart = match host.platform {
        Platform::Linux => linux_restart_script(tag, &host.services),
        Platform::Macos => macos_restart_script(tag, &host.services)?,
    };
    host.run_script("restart the services", &restart)?;

    let keeper_after = host
        .run_script("read the keeper pid", &pid_script(host.platform))?
        .trim()
        .to_owned();
    // A dead keeper reads as `dead:<pid file>`, so a stale pid file that did
    // not change is still a failure: no process is holding the PTYs.
    if keeper_after.starts_with("dead:") || keeper_after != keeper_before {
        return Err(format!(
            "keeper pid changed on {}: {keeper_before} -> {keeper_after}",
            host.name
        ));
    }
    println!(
        "{}: {tag} installed in {}s, keeper {keeper_after} kept",
        host.name,
        started.elapsed().as_secs()
    );
    Ok(())
}

/// Copy the staged pair and bundle into `versions/<tag>/`, then print the
/// binary's version, its build sha and the current keeper pid, one per line.
/// Each binary is renamed into place, so re-running an install never writes
/// into a running executable.
fn place_script(platform: Platform, tag: &str) -> String {
    let root = platform.data_root();
    let quarantine = match platform {
        Platform::Macos => "xattr -dr com.apple.quarantine \"$REL\" 2>/dev/null || true\n",
        Platform::Linux => "",
    };
    format!(
        "set -euo pipefail\n\
         STAGE=\"$HOME/{STAGE_DIR}/{tag}\"\n\
         ROOT=\"{root}\"\n\
         REL=\"$ROOT/versions/{tag}\"\n\
         mkdir -p \"$REL/bin\"\n\
         for binary in roost roost-keeper; do\n\
           cp \"$STAGE/bin/$binary\" \"$REL/bin/.$binary.new\"\n\
           chmod 755 \"$REL/bin/.$binary.new\"\n\
           mv -f \"$REL/bin/.$binary.new\" \"$REL/bin/$binary\"\n\
         done\n\
         rm -rf \"$REL/web\"\n\
         cp -R \"$STAGE/web\" \"$REL/web\"\n\
         {quarantine}\
         \"$REL/bin/roost\" --version\n\
         \"$REL/bin/roost\" version --build\n\
         {KEEPER_PID_LINE}"
    )
}

fn pid_script(platform: Platform) -> String {
    format!("ROOT=\"{}\"\n{KEEPER_PID_LINE}", platform.data_root())
}

/// Repoint each unit at the tag, drop a spent bootstrap token, and restart the
/// units in order (coordinator before worker), each until it is active.
fn linux_restart_script(tag: &str, services: &[String]) -> String {
    let root = Platform::Linux.data_root();
    let units = services.join(" ");
    format!(
        "set -euo pipefail\n\
         UNITS=\"$HOME/.config/systemd/user\"\n\
         for svc in {units}; do\n\
           cp \"$UNITS/$svc.service\" \"$UNITS/$svc.service.bak-{tag}\"\n\
           sed -i -E -e 's#/versions/[^/]+/#/versions/{tag}/#g' -e '/ROOST_BOOTSTRAP_TOKEN=/d' \"$UNITS/$svc.service\"\n\
         done\n\
         systemctl --user daemon-reload\n\
         for svc in {units}; do\n\
           systemctl --user restart \"$svc\"\n\
           for attempt in $(seq 1 30); do\n\
             systemctl --user is-active --quiet \"$svc\" && break\n\
             sleep 1\n\
           done\n\
           systemctl --user is-active \"$svc\"\n\
         done\n\
         FIRST=\"\"\n\
         for svc in {units}; do FIRST=\"$FIRST $(systemctl --user show -p MainPID --value \"$svc\")\"; done\n\
         sleep {STABLE_SECONDS}\n\
         LAST=\"\"\n\
         for svc in {units}; do LAST=\"$LAST $(systemctl --user show -p MainPID --value \"$svc\")\"; done\n\
         if [ \"$FIRST\" != \"$LAST\" ] || echo \"$LAST\" | grep -qw 0; then\n\
           echo \"services did not stay up: MainPID$FIRST ->$LAST\" >&2\n\
           exit 1\n\
         fi\n\
         if [ -L \"$HOME/.local/bin/roost\" ]; then\n\
           ln -sfn \"{root}/versions/{tag}/bin/roost\" \"$HOME/.local/bin/roost\"\n\
         fi\n"
    )
}

/// Rewrite the LaunchAgent for the tag and re-bootstrap it until running.
fn macos_restart_script(tag: &str, services: &[String]) -> Result<String, String> {
    let [label] = services else {
        return Err("a macOS host runs exactly one LaunchAgent".to_owned());
    };
    let root = Platform::Macos.data_root();
    Ok(format!(
        "set -euo pipefail\n\
         REL=\"{root}/versions/{tag}\"\n\
         PLIST=\"$HOME/Library/LaunchAgents/{label}.plist\"\n\
         DOMAIN=\"gui/$(id -u)\"\n\
         cp \"$PLIST\" \"/tmp/{label}.plist.bak-{tag}\"\n\
         plutil -replace ProgramArguments -json \"[\\\"$REL/bin/roost\\\",\\\"worker\\\"]\" \"$PLIST\"\n\
         plutil -replace WorkingDirectory -string \"$REL/bin\" \"$PLIST\"\n\
         plutil -replace EnvironmentVariables.ROOST_WEB_DIST_PATH -string \"$REL/web\" \"$PLIST\"\n\
         plutil -replace EnvironmentVariables.ROOST_COORDINATOR_URL -string \"{COORDINATOR_URL}\" \"$PLIST\"\n\
         for key in GIT_SHA ROOST_GIT_SHA ROOST_BOOTSTRAP_TOKEN; do\n\
           plutil -remove \"EnvironmentVariables.$key\" \"$PLIST\" 2>/dev/null || true\n\
         done\n\
         plutil -replace AbandonProcessGroup -bool true \"$PLIST\"\n\
         plutil -lint \"$PLIST\"\n\
         launchctl bootout \"$DOMAIN/{label}\" 2>/dev/null || true\n\
         sleep 2\n\
         for attempt in 1 2 3 4 5; do\n\
           launchctl bootstrap \"$DOMAIN\" \"$PLIST\" && break\n\
           sleep 2\n\
         done\n\
         for attempt in $(seq 1 30); do\n\
           launchctl print \"$DOMAIN/{label}\" 2>/dev/null | grep -q 'state = running' && break\n\
           sleep 1\n\
         done\n\
         FIRST=$(launchctl print \"$DOMAIN/{label}\" | awk '$1 == \"pid\" {{ print $3; exit }}')\n\
         sleep {STABLE_SECONDS}\n\
         LAST=$(launchctl print \"$DOMAIN/{label}\" | awk '$1 == \"pid\" {{ print $3; exit }}')\n\
         if [ -z \"$FIRST\" ] || [ \"$FIRST\" != \"$LAST\" ]; then\n\
           echo \"{label} did not stay up: pid $FIRST -> $LAST\" >&2\n\
           exit 1\n\
         fi\n"
    ))
}
