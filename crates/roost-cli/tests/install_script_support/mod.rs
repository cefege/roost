//! The sandbox the `install.sh` tests run the real script in: a home, a PATH,
//! stand-in `roost` binaries that log being exec'd, and a release published
//! over `file://`. Used by `install_script` and `install_script_modes`.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::Digest;

/// The script this crate ships and the enrolment command points at. It is one
/// file outside `src/`, so its path is derived rather than restated.
pub fn install_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../install.sh")
        .canonicalize()
        .expect("install.sh exists at the repository root")
}

/// A stand-in for a `roost` binary. `v3` advertises `import-v2` in its help, a
/// subcommand the previous generation has never had; `v2` does not, which is
/// exactly how `install.sh` tells them apart.
pub fn write_fake_roost(path: &Path, is_v3: bool) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory is created");
    let help = if is_v3 {
        "  import-v2  Carry a v2 coordinator's account, devices and keys"
    } else {
        "  status      Health readout"
    };
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n\
             # A stand-in that records being exec'd, so a test can tell a refusal\n\
             # from a hand-off.\n\
             if [ \"$1\" = \"--help\" ]; then echo \"{help}\"; exit 0; fi\n\
             echo \"EXECED $0 $*\" >> \"$ROOST_TEST_JOIN_LOG\"\n\
             exit 0\n"
        ),
    )
    .expect("the fake roost is written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake roost is executable");
}

pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-install-script-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("the home is created");
        Self { root }
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The `PATH` a login-less ssh shell would carry, holding `path_dir`.
    pub fn path(&self, path_dir: &Path) -> String {
        let mut entries = vec![path_dir.display().to_string()];
        for essential in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            entries.push(essential.to_string());
        }
        entries.join(":")
    }

    pub fn log(&self) -> PathBuf {
        self.root.join("execed.log")
    }

    /// A release the script fetches over `file://`: the release list naming one
    /// v3 tag, and every published asset name as a v3 stand-in with its digest
    /// beside it, so the fetch path runs for real without a network. Returns
    /// the release-list URL and the download origin.
    pub fn publish_fake_release(&self) -> (String, String) {
        let release = self.root.join("release");
        let assets = release.join("download/v3.9.9");
        for name in [
            "roost",
            "roost-linux-x64",
            "roost-linux-arm64",
            "roost-darwin-x64",
        ] {
            write_fake_roost(&assets.join(name), true);
            let keeper = assets.join(format!("roost-keeper{}", name.trim_start_matches("roost")));
            std::fs::write(&keeper, b"#!/bin/sh\nexit 0\n").expect("the keeper stand-in");
            for asset in [assets.join(name), keeper] {
                let bytes = std::fs::read(&asset).expect("the asset is readable");
                let digest = hex::encode(sha2::Sha256::digest(&bytes));
                let file_name = asset.file_name().expect("a name").to_string_lossy();
                std::fs::write(
                    assets.join(format!("{file_name}.sha256")),
                    format!("{digest}  {file_name}\n"),
                )
                .expect("the sidecar is written");
            }
        }
        let list = release.join("releases.json");
        std::fs::write(&list, br#"[{"tag_name": "v3.9.9"}]"#).expect("the release list");
        (
            format!("file://{}", list.display()),
            format!("file://{}", release.join("download").display()),
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What one run is handed: the grant, or none, and the arguments after
/// `bash -s --`.
pub struct Invocation<'a> {
    pub grant: bool,
    pub args: &'a [&'a str],
}

/// A pasted Add machine one-liner.
pub const JOIN: Invocation<'static> = Invocation {
    grant: true,
    args: &[],
};

/// Run the script with the environment a pasted one-liner gives it, and return
/// what it wrote, both streams and the exit status.
pub fn run_script(sandbox: &Sandbox, path_dir: &Path) -> (bool, String, String) {
    let api = sandbox.root.join("no-such-api").display().to_string();
    let origin = sandbox.root.join("no-such-origin").display().to_string();
    run_script_against(sandbox, path_dir, &api, &origin, &JOIN)
}

/// The same run, against a named release list and download origin.
pub fn run_script_against(
    sandbox: &Sandbox,
    path_dir: &Path,
    release_api: &str,
    release_origin: &str,
    invocation: &Invocation<'_>,
) -> (bool, String, String) {
    let mut command = std::process::Command::new("bash");
    command
        .arg(install_script())
        .args(invocation.args)
        .env_remove("ROOST_COORDINATOR_URL")
        .env_remove("ROOST_BOOTSTRAP_TOKEN")
        .env_remove("ROOST_BIN")
        .env("HOME", sandbox.home())
        .env("PATH", sandbox.path(path_dir))
        .env("ROOST_TEST_JOIN_LOG", sandbox.log())
        .env("ROOST_RELEASE_BASE_URL", release_origin)
        .env("ROOST_RELEASE_API_URL", release_api)
        .stdin(std::process::Stdio::null());
    if invocation.grant {
        command
            .env("ROOST_COORDINATOR_URL", "https://coordinator.example")
            .env("ROOST_BOOTSTRAP_TOKEN", "roost_bt_test");
    }
    let output = command.output().expect("bash runs the script");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
