//! The two files a resolved POSIX launch contract points at: a bootstrap rcfile
//! that emits OSC 7, and a per-cwd history file. Written by the resolver during
//! resolution, read by bash/zsh and by nothing in this crate. Depends on
//! `sha2` for the history slug and on `roost_protocol`'s own home layout — and
//! on no other module here.
//!
//! WHY A BOOTSTRAP RCFILE AT ALL. The SPA learns a session's folder from the
//! OSC 7 escape the shell prints after every prompt. An rcfile the user never
//! wrote is the only place that emission can be installed without editing
//! anyone's dotfiles, and it sources the real rcfile first so a theme, an alias
//! and a `PATH` still load exactly as they did outside roost.
//!
//! WHY THE HISTORY FILE IS PER-CWD. `↑` recall has to survive a worker restart
//! and a session respawn, so the file lives on disk keyed by a hash of the
//! folder. Sharing one global file across every session would interleave two
//! users' history on one machine.

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The shell whose rcfile the resolver can install the OSC 7 hook into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellFlavour {
    Bash,
    Zsh,
    /// A shell with no bootstrap of its own. It is launched bare.
    Other,
}

impl ShellFlavour {
    /// The flavour of a resolved executable, from its file name alone.
    #[must_use]
    pub fn of(executable: &str) -> Self {
        match file_name_of(executable).as_deref() {
            Some("bash") => Self::Bash,
            Some("zsh") => Self::Zsh,
            _ => Self::Other,
        }
    }
}

/// The last path segment of a POSIX path, with the backslash form folded in
/// because a `SHELL` carried across a platform boundary may still use one.
fn file_name_of(executable: &str) -> Option<String> {
    executable
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The directory a shell's bootstrap lives in: `ZDOTDIR` for zsh, and the
/// directory holding the rcfile bash is pointed at with `--rcfile`.
pub fn bootstrap_dir(root: &Path, flavour: ShellFlavour) -> PathBuf {
    match flavour {
        ShellFlavour::Bash => root.join("roost-bash-osc7"),
        ShellFlavour::Zsh => root.join("roost-zsh-noPROMPT_SP"),
        ShellFlavour::Other => root.to_path_buf(),
    }
}

/// The rcfile itself, written 0600 inside a 0700 directory.
///
/// Both modes are set at creation rather than tightened afterwards: a
/// between-states window on a file a shell sources is a window in which another
/// account on this machine can rewrite what every roost shell runs.
pub fn ensure_bootstrap(root: &Path, flavour: ShellFlavour) -> io::Result<PathBuf> {
    let directory = bootstrap_dir(root, flavour);
    std::fs::create_dir_all(&directory)?;
    let set_mode = |path: &Path, mode: u32| -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    };
    set_mode(&directory, 0o700)?;
    let (name, body) = match flavour {
        ShellFlavour::Bash => ("roost.bashrc", BASH_BOOTSTRAP),
        ShellFlavour::Zsh => (".zshrc", ZSH_BOOTSTRAP),
        ShellFlavour::Other => return Ok(directory),
    };
    let path = directory.join(name);
    std::fs::write(&path, body)?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

const BASH_BOOTSTRAP: &str = r#"# roost: source the user's real bashrc first so PATH/aliases still load
if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi
roost_emit_osc7() { printf '\033]7;file://%s%s\033\\' "${HOSTNAME}" "$PWD"; }
PROMPT_COMMAND="roost_emit_osc7${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
roost_emit_osc7
"#;

const ZSH_BOOTSTRAP: &str = r#"# roost: disable PROMPT_SP so the SPA wterm doesn't see whitespace junk
unsetopt PROMPT_SP PROMPT_CR 2>/dev/null
PROMPT_EOL_MARK=''
# Source the real user zshrc so theme/aliases/path still load
if [ -f "$HOME/.zshrc" ]; then source "$HOME/.zshrc"; fi
function roost_emit_osc7 { print -Pn "\e]7;file://${HOST}${PWD}\e\\" }
autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook chpwd roost_emit_osc7
roost_emit_osc7
"#;

/// The `HISTFILE`/`HISTSIZE`/`SAVEHIST` block a shell session is launched with.
///
/// `HISTSIZE` and `SAVEHIST` are set as well as the file: zsh persists nothing
/// unless both are above zero, and its defaults of 10 would silently truncate a
/// day's work on exit.
#[must_use]
pub fn history_env(cwd: &str, home: &Path) -> Vec<(String, String)> {
    let path = history_path(cwd, home);
    vec![
        ("HISTFILE".to_string(), path),
        ("HISTSIZE".to_string(), HISTORY_ENTRIES.to_string()),
        ("SAVEHIST".to_string(), HISTORY_ENTRIES.to_string()),
    ]
}

/// zsh's own defaults are 10 entries. Long enough that a day of work in one
/// folder is not thrown away when the shell exits.
const HISTORY_ENTRIES: &str = "10000";

/// The on-disk history file for one folder: `~/.roost/history/<slug>.history`.
///
/// The slug is a hash rather than the folder's own name because a folder path
/// contains separators and can be longer than any file name a filesystem
/// accepts, and because a path is not a safe file name to hand to `open`.
#[must_use]
pub fn history_path(cwd: &str, home: &Path) -> String {
    let slug = slug_of(cwd);
    home.join(".roost")
        .join("history")
        .join(format!("{slug}.history"))
        .display()
        .to_string()
}

/// The first twelve hex characters of the SHA-256 of a folder identity.
///
/// The only digest renderer in this module, and it exists because the
/// workspace has no shared one: the length is what makes the file name short,
/// and SHA-256 is what makes two folders with similar names not collide.
fn slug_of(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
