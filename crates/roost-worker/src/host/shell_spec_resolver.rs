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
//! Ports v2 `apps/worker/src/shell-spec.ts`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use roost_host::HostPlatform;

use crate::host::shell_bootstrap::{self, ShellFlavour};
use crate::host::shell_locate::{environment_variable, locate_shell, shell_candidates};
pub use crate::host::tool_path::PTY_PATH_PREFIX;
use crate::session::spawn::{SessionEnvironmentOverlay, ShellSpecResolver};
use crate::shell_spec::{
    SESSION_ID_ENV, SHELL_SPEC_VERSION, ShellSpec, is_keeper_control_key, is_worker_private_key,
};

/// The terminal the SPA's renderer is written against.
pub const PTY_TERM: &str = "xterm-256color";

/// What a PTY is told it may use colour, so a tool does not have to guess.
pub const PTY_COLORTERM: &str = "truecolor";

/// The locale a PTY gets when the service environment names none.
pub const DEFAULT_DARWIN_LOCALE: &str = "en_US.UTF-8";
pub const DEFAULT_LINUX_LOCALE: &str = "C.UTF-8";

/// The temporary root the bootstrap rcfile is written under.
///
/// From the environment rather than `std::env::temp_dir()` so a test resolves
/// into its own scratch and a service whose `TMPDIR` is a private directory
/// keeps the file out of a world-writable one. Windows names it `TEMP`/`TMP`.
const TMPDIR_ENV: &str = "TMPDIR";
const DEFAULT_TMPDIR: &str = "/tmp";
const WINDOWS_TEMP_ENVS: [&str; 2] = ["TEMP", "TMP"];
const HOME_ENV: &str = "HOME";
const USERPROFILE_ENV: &str = "USERPROFILE";
const PATH_ENV: &str = "PATH";
const PATHEXT_ENV: &str = "PATHEXT";

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
    overlay: Option<Arc<dyn SessionEnvironmentOverlay>>,
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
            overlay: None,
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
    pub fn with_overlay(mut self, overlay: Arc<dyn SessionEnvironmentOverlay>) -> Self {
        self.overlay = Some(overlay);
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
            ShellFlavour::Bash | ShellFlavour::Zsh => Some(self.ensure_bootstrap(flavour)?),
            ShellFlavour::PowerShell | ShellFlavour::Cmd | ShellFlavour::Other => None,
        };

        let mut env = self.base_environment();
        for (key, value) in self.common_environment(platform) {
            env.insert(key, value);
        }
        // Windows keeps the inherited `Path` as it is: there is no package
        // manager prefix to add, and a second spelling of the key would leave
        // the child two values to choose between.
        if platform != HostPlatform::Windows {
            env.insert(PATH_ENV.into(), self.pty_path());
        }
        let home = self.variable(HOME_ENV).or_else(|| {
            (platform == HostPlatform::Windows)
                .then(|| self.variable(USERPROFILE_ENV))
                .flatten()
        });
        if let Some(home) = home {
            env.extend(shell_bootstrap::history_env(&cwd, Path::new(&home)));
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
            (_, ShellFlavour::PowerShell) => vec![
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-EncodedCommand".to_string(),
                shell_bootstrap::encode_powershell_command(
                    shell_bootstrap::powershell_bootstrap_script(),
                ),
            ],
            _ => Vec::new(),
        };
        let overlay = match &self.overlay {
            Some(overlay) => overlay.session_overlay(session_id)?,
            None => Vec::new(),
        };
        for (key, value) in overlay {
            if is_keeper_control_key(&key) {
                tracing::warn!(
                    key = %key,
                    session_id = %session_id,
                    "a caller-supplied overlay named a keeper control credential; it was dropped \
                     rather than inherited into the PTY"
                );
                continue;
            }
            env.insert(key, value);
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
        Ok(self.platform)
    }

    fn variable(&self, key: &str) -> Option<String> {
        environment_variable(&self.environment, self.platform, key)
    }

    /// The inherited environment minus the whole `ROOST_` namespace: the
    /// worker's label, coordinator, bootstrap token, data dirs and keeper
    /// credentials, which let a shell start a worker that impersonates this one.
    /// The overlay and [`SESSION_ID_ENV`], applied after, re-add the session's
    /// keys. [`is_worker_private_key`] is case-insensitive, unlike a prefix test.
    fn base_environment(&self) -> BTreeMap<String, String> {
        self.environment
            .iter()
            .filter(|(key, _)| !is_worker_private_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The variables a PTY is given whether or not the service had them.
    ///
    /// Set explicitly rather than inherited: a worker's `TERM` is `dumb` under
    /// a service manager, and `LANG` is often `C`, and both of those reach the
    /// shell as the user's own answer rather than as this daemon's.
    fn common_environment(&self, platform: HostPlatform) -> Vec<(String, String)> {
        // NOT inherited, and this is the whole point of the function. The
        // doc comment above says a worker's `LANG` is "often `C`" and that
        // reaching the shell is how a browser renders a shell that cannot
        // draw — and then the code below read `LANG` back out of the
        // environment and shipped that `C` straight through. The comment and
        // the code disagreed and the code won, because nothing ran.
        //
        // The default is per-platform, not per-machine, so it does not vary
        // with whoever happened to launch the daemon.
        let mut common = vec![
            ("TERM".to_string(), PTY_TERM.to_string()),
            ("COLORTERM".to_string(), PTY_COLORTERM.to_string()),
        ];
        // Windows has no locale variables a console program reads; its code
        // page is the PowerShell bootstrap's to set.
        if platform == HostPlatform::Windows {
            return common;
        }
        let locale = match platform {
            HostPlatform::MacOs => DEFAULT_DARWIN_LOCALE.to_string(),
            _ => DEFAULT_LINUX_LOCALE.to_string(),
        };
        common.extend([
            ("LANG".to_string(), locale.clone()),
            ("LC_ALL".to_string(), locale),
            // A marker the shell prints at end-of-line when PROMPT_SP is on;
            // the SPA's grid treats it as junk.
            ("PROMPT_EOL_MARK".to_string(), String::new()),
        ]);
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
        let inherited = self
            .variable(PATH_ENV)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if self.platform == HostPlatform::Windows {
            return inherited.unwrap_or_default();
        }
        match inherited {
            Some(inherited) => format!("{PTY_PATH_PREFIX}:{inherited}"),
            None => PTY_PATH_PREFIX.to_string(),
        }
    }

    /// The absolute path of the shell this session runs, or the reason there is
    /// none. Refusing here is what turns a keeper-side ENOENT into a reason.
    fn resolve_executable(&self) -> Result<String, String> {
        let candidates = shell_candidates(self.platform, &|key| self.variable(key));
        let search_path = self.pty_path();
        for candidate in &candidates {
            if let Some(path) = locate_shell(
                self.platform,
                &search_path,
                self.variable(PATHEXT_ENV),
                candidate,
            ) {
                return Ok(path);
            }
        }
        Err(format!(
            "no shell was found to launch: {} resolved to no executable file on this host",
            candidates.join(", ")
        ))
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
        let root = self.bootstrap_root();
        let path = shell_bootstrap::ensure_bootstrap(&root, flavour).map_err(|error| {
            format!(
                "the shell bootstrap could not be written under {}: {error}",
                root.display()
            )
        })?;
        written.insert(flavour, path.clone());
        Ok(path)
    }

    /// Where a bootstrap rcfile is written: `TMPDIR` (`/tmp` when unset) on
    /// Unix, `TEMP` then `TMP` (the process temp directory when neither is
    /// set) on Windows.
    fn bootstrap_root(&self) -> PathBuf {
        let named = |key: &str| {
            self.variable(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        if self.platform == HostPlatform::Windows {
            return WINDOWS_TEMP_ENVS
                .iter()
                .find_map(|key| named(key))
                .map_or_else(std::env::temp_dir, PathBuf::from);
        }
        PathBuf::from(named(TMPDIR_ENV).unwrap_or_else(|| DEFAULT_TMPDIR.to_string()))
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
