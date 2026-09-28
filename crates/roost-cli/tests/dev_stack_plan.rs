//! What `roost dev` starts: the coordinator the loader resolved, the worker
//! pointed at exactly that one, and the Dioxus CLI serving the workspace
//! member.
//!
//! The assertions are about agreement rather than about a copy of the argv. A
//! dev worker dialling a port the dev coordinator is not on produces a stack
//! that looks healthy and reaches nothing, and only the RESOLVED values can say
//! whether the two agree — which is also why the bind is read out of the
//! coordinator's own boot instead of being spelled here a second time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use roost_cli::dev::DevBoot;
use roost_cli::dev::plan::{self, DevServer};
use roost_cli::dev::resolve_dev_boot;
use roost_host::{DEFAULT_COORDINATOR_BIND, HostPlatform, MapEnv};

/// The `roost` an operator would be running, as far as a plan is concerned.
const ROOST: &str = "/usr/local/bin/roost";

fn home() -> MapEnv {
    MapEnv::new().with("HOME", "/tmp/roost-dev-plan-test")
}

fn dev_boot(env: &MapEnv) -> DevBoot {
    resolve_dev_boot(env, HostPlatform::Linux).expect("a dev boot with only a home declared")
}

fn plan_for(boot: &DevBoot) -> Vec<DevServer> {
    plan::dev_plan(Path::new(ROOST), &boot.coordinator_url)
}

fn named<'a>(servers: &'a [DevServer], name: &str) -> &'a DevServer {
    servers
        .iter()
        .find(|server| server.name == name)
        .unwrap_or_else(|| panic!("{name} is not in the dev plan"))
}

/// The value of a flag, so a test can assert on what a child was told rather
/// than on the position of an argument.
fn flag_value(server: &DevServer, flag: &str) -> String {
    let at = server
        .args
        .iter()
        .position(|argument| argument == flag)
        .unwrap_or_else(|| panic!("{} was not started with {flag}", server.name));
    server
        .args
        .get(at + 1)
        .unwrap_or_else(|| {
            panic!(
                "{} was started with {flag} and nothing after it",
                server.name
            )
        })
        .clone()
}

#[test]
fn the_dev_worker_dials_the_coordinator_the_loader_resolved() {
    let boot = dev_boot(&home());
    let servers = plan_for(&boot);

    // The number has exactly one owner: the product default every `roost coord`
    // with no flags also binds, so a dev stack cannot quietly become the only
    // thing listening on the port the product documents.
    assert_eq!(boot.coordinator_bind, DEFAULT_COORDINATOR_BIND);
    assert_eq!(
        boot.coordinator_url,
        format!("http://{DEFAULT_COORDINATOR_BIND}")
    );
    assert_eq!(
        flag_value(named(&servers, plan::WORKER), "--coordinator-url"),
        boot.coordinator_url
    );
}

#[test]
fn an_operator_declared_bind_is_the_one_both_servers_agree_on() {
    // The dev coordinator resolves the same environment this process did, so a
    // flag on the coordinator child would let the two disagree here — and the
    // disagreement is invisible until a browser reaches a stack that is not the
    // one it was started with.
    let boot = dev_boot(&home().with("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"));
    let servers = plan_for(&boot);

    assert_eq!(boot.coordinator_bind, "127.0.0.1:4200");
    assert_eq!(
        flag_value(named(&servers, plan::WORKER), "--coordinator-url"),
        "http://127.0.0.1:4200"
    );
    let coordinator = named(&servers, plan::COORDINATOR);
    assert!(
        !coordinator.args.iter().any(|argument| argument == "--bind"),
        "the coordinator child must resolve the bind the parent resolved, not be told a different one: {:?}",
        coordinator.args
    );
}

#[test]
fn a_configuration_the_loader_refuses_stops_the_command_before_anything_starts() {
    // Half a Cloudflare Access pair is a coordinator that answers without
    // authentication, so `roost-host` refuses it. Three children each dying on
    // the same refusal is a worse report than one failure line.
    let env = home().with("ROOST_CF_ACCESS_TEAM_DOMAIN", "roost.cloudflare-access.com");
    let failure = resolve_dev_boot(&env, HostPlatform::Linux)
        .expect_err("half a Cloudflare Access pair was accepted");

    assert!(
        failure.message.contains("ROOST_CF_ACCESS_TEAM_DOMAIN"),
        "the refusal must name the variable to fix: {}",
        failure.message
    );
}

#[test]
fn the_web_server_is_the_dioxus_cli_on_the_workspace_member() {
    // v3's web app is a Rust crate, so the dev server is the Dioxus CLI serving
    // the same package the release build names. A vite child here would be a
    // v2 port of a line that no longer exists.
    let servers = plan_for(&dev_boot(&home()));
    let web = named(&servers, plan::WEB);

    assert_eq!(web.program.to_string_lossy(), plan::WEB_PROGRAM);
    assert_eq!(flag_value(web, "-p"), plan::WEB_PACKAGE);
    assert_eq!(flag_value(web, "--platform"), "web");
}

#[test]
fn resolving_a_dev_boot_exports_nothing_into_this_process() {
    // The leak this guards is invisible in a test that only reads the returned
    // value: a dev bind that reached the loader by being set in the ambient
    // environment would still be there for the next command in this shell.
    let declared = std::env::var("ROOST_COORDINATOR_BIND").ok();
    let boot = dev_boot(&home());
    let _ = plan_for(&boot);
    assert_eq!(declared, std::env::var("ROOST_COORDINATOR_BIND").ok());
    assert!(
        declared
            .as_deref()
            .is_none_or(|value| value != boot.coordinator_bind),
        "the bind this process resolved was already in its own environment: {declared:?}"
    );
}
