//! The folder → launch contract resolution: the one thing between a browser's
//! request for a terminal and a keeper holding a PTY. Implements
//! [`crate::session::spawn::ShellSpecResolver`], which `session::lifecycle`
//! borrows as `Arc<dyn ShellSpecResolver>`; the resolved [`ShellSpec`] is
//! retained on the record for a respawn. Depends on `roost_host` for the
//! platform, on `host::shell_bootstrap` for the two files a shell reads, and on
//! `crate::shell_spec` for the contract itself.
//!
//! EVERY REFUSAL HAPPENS HERE, BEFORE THE KEEPER IS TOLD ANYTHING. A missing
//! `SHELL`, a folder that cannot be created, a platform that is not this host:
//! each is a reason the caller can read. The same three reached through a
//! keeper's own process boundary are ENOENTs on a socket the caller never sees
//! the inside of, and a browser is left watching a session that will never
//! arrive.
//!
//! NO KEEPER CONTROL CREDENTIAL REACHES A PTY. The strip is case-insensitive on
//! purpose and it runs over the overlays as well as the inherited environment:
//! a worker that leaks one hands every command the user types the ability to
//! speak to the keeper as this worker, which is every terminal on the machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use roost_host::HostPlatform;

use crate::host::shell_bootstrap::{self, ShellFlavour};
use crate::host::tool_path::PTY_PATH_PREFIX;
use crate::session::spawn::ShellSpecResolver;
use crate::shell_spec::{SESSION_ID_ENV, SHELL_SPEC_VERSION, ShellSpec, is_keeper_control_key};

/// The terminal the SPA's renderer is written against.
pub const PTY_TERM: &str = "xterm-256color";

/// What a PTY is told it may use colour, so a tool does not have to guess.
pub const PTY_COLORTERM: &str = "truecolor";

/// The locale a PTY gets when the service environment names none.
pub const DEFAULT_DARWIN_LOCALE: &str = "en_US.UTF-8";
pub const DEFAULT_LINUX_LOCALE: &str = "C.UTF-8";

/// The shells tried, in order, when the environment names no `SHELL`.
///
/// bash first because it is the POSIX default and is present on every supported
/// host; `sh` last because it is the only one guaranteed by POSIX itself, and
/// it is the fallback that keeps a minimal container from refusing to spawn.
const FALLBACK_SHELLS: [&str; 2] = ["/bin/bash", "/bin/sh"];

/// The temporary root the bootstrap rcfile is written under.
///
/// From the environment rather than `std::env::temp_dir()` so a test resolves
/// into its own scratch and a service whose `TMPDIR` is a private directory
/// keeps the file out of a world-writable one.
const TMPDIR_ENV: &str = "TMPDIR";
const DEFAULT_TMPDIR: &str = "/tmp";
const HOME_ENV: &str = "HOME";
const SHELL_ENV: &str = "SHELL";
const PATH_ENV: &str = "PATH";

/// This host's environment, snapshotted, and the platform a session was asked
/// for.
///
/// The environment is a value rather than a live view because a resolution must
/// be reproducible: a spec built from a moving environment is a spec whose
/// bytes differ between the record and the respawn that reads it back.
pub struct HostShellSpecResolver {
    environment: BTreeMap<String, String>,
    /// The platform this process runs on, resolved once at construction.
    platform: HostPlatform,
    /// The platform the caller asked for. Equal to `platform` in production;
    /// a mismatch is refused rather than resolved for the wrong host.
    requested_platform: HostPlatform,
    /// Per-session variables a caller contributes — the agent report endpoint
    /// and its capability, which are derived from the session id and so cannot
    /// be read from the environment.
    overlay: BTreeMap<String, String>,
    /// The rcfile written per flavour, remembered so a second session in the
    /// same worker does not rewrite a file a live shell is reading.
    bootstraps: Mutex<BTreeMap<ShellFlavour, PathBuf>>,
}

impl HostShellSpecResolver {
    /// A resolver for one environment and one host.
    ///
    /// `requested_platform` is taken rather than read so the refusal for a
    /// mismatched request is testable on any machine, and so a caller that has
    /// a platform in hand — a deploy manifest, a stored record — cannot
    /// accidentally resolve against the host's own answer instead.
    #[must_use]
    pub fn new(
        environment: BTreeMap<String, String>,
        platform: HostPlatform,
        requested_platform: HostPlatform,
    ) -> Self {
        Self {
            environment,
            platform,
            requested_platform,
            overlay: BTreeMap::new(),
            bootstraps: Mutex::new(BTreeMap::new()),
        }
    }

    /// A resolver for this host, accepting only this host's sessions.
    ///
    /// The refusal is a `String` rather than a typed error because the trait
    /// this implements carries one, and a second error type for the same
    /// condition is a second answer to it.
    pub fn for_this_host(environment: BTreeMap<String, String>) -> Result<Self, String> {
        let platform = roost_host::supported_host_platform()
            .map_err(|error| format!("this host's platform is not one v3 runs on: {error}"))?;
        Ok(Self::new(environment, platform, platform))
    }

    /// The whole process environment, for the one caller that has no other way
    /// to enumerate it.
    #[must_use]
    pub fn inherited_process_environment() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }

    /// Add the per-session variables a caller contributes to every spec.
    ///
    /// Filtered by the same keeper strip as the inherited environment, because
    /// a caller is no more trusted than the environment is: an overlay that
    /// named a keeper credential is the same leak with one more hop in it.
    #[must_use]
    pub fn with_overlay(mut self, overlay: impl IntoIterator<Item = (String, String)>) -> Self {
        self.overlay = overlay.into_iter().collect();
        self
    }

    /// The launch contract for a folder, or the reason there is not one.
    pub fn resolve(&self, cwd: &str, session_id: &str) -> Result<ShellSpec, String> {
        let platform = self.check_platform()?;
        if cwd.trim().is_empty() {
            return Err("a session needs a folder to open its terminal in".to_string());
        }
        let directory = PathBuf::from(cwd);
        // Materialised here, not at the PTY. A keeper asked to open a shell in a
        // directory that does not exist answers with a spawn error three frames
        // later, and the caller has already been told the session is opening.
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("the session folder {cwd} could not be created: {error}"))?;
        let cwd = directory.display().to_string();

        let executable = self.resolve_executable()?;
        let flavour = ShellFlavour::of(&executable);
        let bootstrap = match flavour {
            ShellFlavour::Other => None,
            other => Some(self.ensure_bootstrap(other)?),
        };

        let mut env = self.base_environment();
        for (key, value) in self.common_environment(platform) {
            env.insert(key, value);
        }
        env.insert(PATH_ENV.into(), self.pty_path());
        if let Some(home) = self.environment.get(HOME_ENV) {
            env.extend(shell_bootstrap::history_env(&cwd, Path::new(home)).into_iter());
        }
        // A shell that is told where its real rcfile is, and told to load it.
        let argv = match (&bootstrap, flavour) {
            (Some(path), ShellFlavour::Bash) => {
                vec!["--rcfile".to_string(), path.display().to_string()]
            }
            (Some(path), ShellFlavour::Zsh) => {
                env.insert(
                    "ZDOTDIR".into(),
                    path.parent().unwrap_or(path).display().to_string(),
                );
                Vec::new()
            }
            _ => Vec::new(),
        };
        for (key, value) in &self.overlay {
            if is_keeper_control_key(key) {
                tracing::warn!(
                    key = %key,
                    session_id = %session_id,
                    "a caller-supplied overlay named a keeper control credential; it was dropped \
                     rather than inherited into the PTY"
                );
                continue;
            }
            env.insert(key.clone(), value.clone());
        }
        // Last, so nothing a caller supplied can move the identity out from
        // under the record this spec is about to be attached to.
        env.insert(SESSION_ID_ENV.into(), session_id.to_string());

        let spec = ShellSpec {
            version: SHELL_SPEC_VERSION,
            platform,
            executable: executable.clone(),
            argv,
            cwd: cwd.clone(),
            env: env.into_iter().collect(),
        };
        tracing::info!(
            %session_id,
            %cwd,
            %executable,
            shell = ?flavour,
            platform = platform.as_str(),
            variables = spec.env.len(),
            "a launch contract was resolved for a session folder"
        );
        Ok(spec)
    }

    /// The platform this resolver may build a spec for, or the reason it may not.
    fn check_platform(&self) -> Result<HostPlatform, String> {
        if self.platform != self.requested_platform {
            return Err(format!(
                "a session was requested for {} but this worker runs on {}",
                self.requested_platform.as_str(),
                self.platform.as_str()
            ));
        }
        if self.platform == HostPlatform::Windows {
            return Err(
                "Roost v3 opens no Windows terminal; this worker runs on macOS and Linux only"
                    .to_string(),
            );
        }
        Ok(self.platform)
    }

    /// The inherited environment, with every keeper control credential removed.
    ///
    /// The filter is [`is_keeper_control_key`] rather than a prefix test of its
    /// own: a case-sensitive comparison waves `Roost_Keeper_Capability` straight
    /// through, and this predicate is guarded by its own mutation experiment.
    fn base_environment(&self) -> BTreeMap<String, String> {
        self.environment
            .iter()
            .filter(|(key, _)| !is_keeper_control_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The variables a PTY is given whether or not the service had them.
    ///
    /// Set explicitly rather than inherited: a worker's `TERM` is `dumb` under
    /// a service manager, and `LANG` is often `C`, and both of those reach the
    /// shell as the user's own answer rather than as this daemon's.
    fn common_environment(&self, platform: HostPlatform) -> Vec<(String, String)> {
        let locale = self
            .environment
            .get("LANG")
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| {
                self.environment
                    .get("LC_ALL")
                    .filter(|v| !v.is_empty())
                    .cloned()
            })
            .unwrap_or_else(|| match platform {
                HostPlatform::MacOs => DEFAULT_DARWIN_LOCALE.to_string(),
                _ => DEFAULT_LINUX_LOCALE.to_string(),
            });
        let mut common = vec![
            ("TERM".to_string(), PTY_TERM.to_string()),
            ("COLORTERM".to_string(), PTY_COLORTERM.to_string()),
            ("LANG".to_string(), locale.clone()),
            ("LC_ALL".to_string(), locale),
            // A marker the shell prints at end-of-line when PROMPT_SP is on;
            // the SPA's grid treats it as junk.
            ("PROMPT_EOL_MARK".to_string(), String::new()),
        ];
        if platform == HostPlatform::MacOs {
            common.push(("TERM_PROGRAM".to_string(), "Apple_Terminal".to_string()));
        }
        common
    }

    /// The `PATH` a PTY is launched with: the package managers' directories in
    /// front of the inherited one.
    ///
    /// This is the `gh`/`lsof` fix. The worker's launchd and systemd units carry
    /// a minimal `PATH`, so a shell spawned from this spec without the prefix
    /// cannot run `gh` at all, and the PR badge on a folder row silently never
    /// resolves — no error, no log, just a missing badge.
    fn pty_path(&self) -> String {
        match self
            .environment
            .get(PATH_ENV)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            Some(inherited) => format!("{PTY_PATH_PREFIX}:{inherited}"),
            None => PTY_PATH_PREFIX.to_string(),
        }
    }

    /// The absolute path of the shell this session runs, or the reason there is
    /// none. Refusing here is what turns a keeper-side ENOENT into a reason.
    fn resolve_executable(&self) -> Result<String, String> {
        let configured = self
            .environment
            .get(SHELL_ENV)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let candidates: Vec<String> = match configured {
            Some(configured) => vec![configured],
            None => FALLBACK_SHELLS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        };
        for candidate in &candidates {
            if let Some(path) = self.locate(candidate) {
                return Ok(path);
            }
        }
        Err(format!(
            "no shell was found to launch: {} resolved to no executable file on this host",
            candidates.join(", ")
        ))
    }

    /// One candidate resolved against `PATH`, or `None`.
    ///
    /// An exec bit is required, not just a file: a `SHELL` pointing at a data
    /// file resolves on every other check and fails at the exec.
    fn locate(&self, candidate: &str) -> Option<String> {
        if candidate.contains('/') {
            return is_executable_file(Path::new(candidate)).then(|| candidate.to_string());
        }
        self.pty_path()
            .split(':')
            .filter(|entry| !entry.is_empty())
            .map(|directory| Path::new(directory).join(candidate))
            .find(|path| is_executable_file(path))
            .map(|path| path.display().to_string())
    }

    /// The rcfile for this shell, written once per resolver.
    fn ensure_bootstrap(&self, flavour: ShellFlavour) -> Result<PathBuf, String> {
        let mut written = self
            .bootstraps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(path) = written.get(&flavour) {
            return Ok(path.clone());
        }
        let root = self
            .environment
            .get(TMPDIR_ENV)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_TMPDIR);
        let path =
            shell_bootstrap::ensure_bootstrap(Path::new(root), flavour).map_err(|error| {
                format!("the shell bootstrap could not be written under {root}: {error}")
            })?;
        written.insert(flavour, path.clone());
        Ok(path)
    }
}

impl std::fmt::Debug for HostShellSpecResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostShellSpecResolver")
            .field("platform", &self.platform)
            .field("requested_platform", &self.requested_platform)
            .field("inherited_variables", &self.environment.len())
            .finish_non_exhaustive()
    }
}

impl ShellSpecResolver for HostShellSpecResolver {
    fn resolve_shell_spec(&self, cwd: &str, session_id: &str) -> Result<ShellSpec, String> {
        self.resolve(cwd, session_id)
    }
}

/// Whether a path is a file this process may execute.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    metadata.permissions().mode() & 0o111 != 0
}
