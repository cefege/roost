//! The web bundle an install carries beside its executables: where it lands,
//! what makes a directory a bundle, and what happens to a release's bundle
//! when that release is retired.
//!
//! The properties worth pinning are the ones an operator finds out about
//! through a browser: a coordinator answering 404 for every URL, a page whose
//! hashed assets 404 because the previous release's files are still being
//! served alongside the new ones, and a retired release whose bundle outlived
//! it and kept a stale UI alive.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::deploy::retire::retire_prior_release;
use roost_cli::services::web_bundle::{
    WEB_DIR_NAME, WEB_INDEX, install_from_dir, install_from_tarball, release_web_dir, validate,
};
use roost_cli::update::release::keeper_release_asset_name;
use roost_host::HostPlatform;

/// A throwaway directory that removes itself.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-web-bundle-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the scratch directory is created");
        Self { root }
    }

    /// A minimal bundle: an index and one asset under a subdirectory, which is
    /// the shape a real bundler emits.
    fn bundle(&self, name: &str, marker: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(dir.join("assets")).expect("the bundle directory is created");
        std::fs::write(dir.join(WEB_INDEX), format!("<html>{marker}</html>\n"))
            .expect("the index is written");
        std::fs::write(dir.join("assets/app.js"), format!("// {marker}\n"))
            .expect("the asset is written");
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_bundle_is_the_release_directory_sibling_of_bin() {
    let bin = Path::new("/home/op/.local/share/RoostWorkerV3/service/versions/3.0.0/bin");
    let web = release_web_dir(bin);
    assert_eq!(web.parent(), bin.parent(), "beside bin/, not inside it");
    assert!(web.ends_with(WEB_DIR_NAME));
}

#[test]
fn a_directory_without_an_index_is_not_a_bundle_and_the_refusal_names_it() {
    let scratch = Scratch::new("no-index");
    let assets_only = scratch.root.join("assets-only");
    std::fs::create_dir_all(assets_only.join("assets")).expect("the directory is created");
    std::fs::write(assets_only.join("assets/app.js"), b"//\n").expect("a file is written");

    let failure = validate(&assets_only).expect_err("a directory of assets is not a bundle");
    let message = failure.to_string();
    assert!(
        message.contains("index.html"),
        "the refusal has to name the file that is missing, or the operator builds the bundle \
         again and gets the same answer: {message}"
    );

    let missing = scratch.root.join("not-there");
    assert!(
        validate(&missing).is_err(),
        "a path that is not there is not a bundle"
    );
}

#[test]
fn an_install_replaces_rather_than_merges_so_a_stale_asset_cannot_survive() {
    let scratch = Scratch::new("replace");
    let first = scratch.bundle("first", "one");
    let second = scratch.bundle("second", "two");
    let destination = scratch.root.join("versions/3.0.0").join(WEB_DIR_NAME);

    let one = install_from_dir(&first, &destination).expect("the first install lands");
    assert_eq!(one.root, destination);
    assert_eq!(one.files, 2, "an index and one nested asset");
    assert!(destination.join(WEB_INDEX).is_file());
    assert!(
        destination.join("assets/app.js").is_file(),
        "a nested directory is copied whole, or the page loads and then fetches nothing"
    );

    std::fs::remove_dir_all(second.join("assets")).expect("the second bundle drops its asset");
    install_from_dir(&second, &destination).expect("the second install lands");
    assert!(
        !destination.join("assets/app.js").exists(),
        "a merged bundle keeps serving the previous release's asset, and an index that references \
         a hash the new build never emitted is a page that loads and then fails for its code"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join(WEB_INDEX)).expect("readable"),
        "<html>two</html>\n",
        "the destination holds the source's bytes, not a mixture"
    );
}

#[test]
fn a_refused_install_leaves_the_previous_bundle_serving() {
    let scratch = Scratch::new("refuse");
    let good = scratch.bundle("good", "good");
    let destination = scratch.root.join("versions/3.0.0").join(WEB_DIR_NAME);
    install_from_dir(&good, &destination).expect("the first install lands");

    let not_a_bundle = scratch.root.join("broken");
    std::fs::create_dir_all(&not_a_bundle).expect("the directory is created");
    let failure =
        install_from_dir(&not_a_bundle, &destination).expect_err("a broken source is refused");
    assert!(failure.to_string().contains(WEB_INDEX));

    assert_eq!(
        std::fs::read_to_string(destination.join(WEB_INDEX)).expect("readable"),
        "<html>good</html>\n",
        "a refused install must not have taken the working bundle with it"
    );
}

#[test]
fn a_release_tarball_installs_from_either_layout_the_pipeline_might_emit() {
    for (label, wrap) in [("flat", false), ("wrapped", true)] {
        let scratch = Scratch::new(label);
        let source = scratch.bundle("src", label);
        // The wrapped layout puts the bundle one directory down, which is the
        // shape an install must not depend on being absent.
        let wrapper = scratch.root.join("wrap");
        let staged = if wrap {
            std::fs::create_dir_all(&wrapper).expect("the wrapper is created");
            install_from_dir(&source, &wrapper.join(WEB_DIR_NAME))
                .expect("the wrapped fixture is built");
            wrapper
        } else {
            source.clone()
        };
        let archive = scratch.root.join("roost-web.tar.gz");
        let tar = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staged)
            .arg(".")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("tar runs");
        assert!(
            tar.status.success(),
            "the fixture archive is built: {label}"
        );

        let destination = scratch.root.join("versions/3.0.0").join(WEB_DIR_NAME);
        let installed = install_from_tarball(&archive, &destination)
            .unwrap_or_else(|error| panic!("the {label} layout installs: {error}"));
        assert_eq!(installed.root, destination);
        assert_eq!(
            std::fs::read_to_string(destination.join(WEB_INDEX)).expect("readable"),
            format!("<html>{label}</html>\n"),
            "the bundle root is found by its index, not by a prefix the pipeline may change"
        );
    }
}

#[test]
fn an_archive_with_no_index_anywhere_is_refused_rather_than_guessed_at() {
    let scratch = Scratch::new("ambiguous");
    let archive = scratch.root.join("roost-web.tar.gz");
    let inner = scratch.root.join("inner");
    std::fs::create_dir_all(inner.join("one")).expect("a directory is created");
    std::fs::create_dir_all(inner.join("two")).expect("a directory is created");
    std::fs::write(inner.join("one/index.html"), b"one\n").expect("a file is written");
    std::fs::write(inner.join("two/index.html"), b"two\n").expect("a file is written");
    let tar = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&inner)
        .args(["one", "two"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("tar runs");
    assert!(tar.status.success());

    let destination = scratch.root.join("versions/3.0.0").join(WEB_DIR_NAME);
    let failure = install_from_tarball(&archive, &destination)
        .expect_err("two candidate roots is not a bundle, it is a coin toss");
    assert!(
        failure.to_string().contains(WEB_INDEX),
        "the refusal names what it was looking for: {failure}"
    );
    assert!(!destination.exists(), "nothing was installed");
}

#[test]
fn retiring_a_release_retires_its_bundle_with_it() {
    let scratch = Scratch::new("retire");
    let release_root = scratch.root.join("versions");
    let prior = release_root.join("3.0.0");
    let bundle = scratch.bundle("bundle", "old");
    let bin = prior.join("bin");
    std::fs::create_dir_all(&bin).expect("the bin directory is created");
    std::fs::write(bin.join("roost"), b"binary\n").expect("the binary is written");
    let web = release_web_dir(&bin);
    install_from_dir(&bundle, &web).expect("the bundle installs into the release");

    let outcome = retire_prior_release(&release_root, &prior)
        .unwrap_or_else(|error| panic!("a release inside the release root retires: {error}"));
    assert!(matches!(
        outcome,
        roost_cli::deploy::retire::Retirement::PlainDirectory
    ));
    assert!(!prior.exists(), "the whole release directory goes");
    assert!(
        !web.exists(),
        "a bundle that outlived its release is how an install keeps serving a UI it stopped \\
         running: {}",
        web.display()
    );
}

#[test]
fn the_keeper_asset_is_the_roost_name_with_the_program_substituted_once() {
    for (platform, arch, expected) in [
        (HostPlatform::Linux, "x86_64", "roost-keeper-linux-x64"),
        (HostPlatform::Linux, "aarch64", "roost-keeper-linux-arm64"),
        (HostPlatform::MacOs, "aarch64", "roost-keeper"),
        (HostPlatform::MacOs, "x86_64", "roost-keeper-darwin-x64"),
    ] {
        assert_eq!(
            keeper_release_asset_name(platform, arch).expect("a keeper is published for this pair"),
            expected,
            "the release pipeline emits one matrix entry and both names; a keeper name invented \\
             here is a 404 on one architecture and not another"
        );
    }
    // Every published name keeps its platform and arch suffix, which is what
    // distinguishes a substitution from a second hand-written table: a table
    // that transposed two rows would still be four plausible-looking names.
    for (platform, arch, suffix) in [
        (HostPlatform::Linux, "x86_64", "-linux-x64"),
        (HostPlatform::Linux, "aarch64", "-linux-arm64"),
        (HostPlatform::MacOs, "aarch64", ""),
        (HostPlatform::MacOs, "x86_64", "-darwin-x64"),
    ] {
        let name = keeper_release_asset_name(platform, arch).expect("a keeper is published");
        assert_eq!(name, format!("roost-keeper{suffix}"), "{platform} {arch}");
    }
}
