//! A setting that is declared but blank is a setting that is not configured.
//! The suite drives the loader at the environment boundary, so every assertion
//! is what a booting coordinator would see. It lives beside `coord_config.rs`
//! rather than inside it because that file is at the 400-line cap and this is
//! one rule rather than one field.
// A behaviour test unwraps the value it is asserting about: a failure
//! there is the assertion failing, which is exactly what a test wants. The
//! workspace denies `unwrap`/`expect` because a panic on a bad wire value in
//! a running component is a fleet-visible outage, and that reasoning does not
//! reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::{
    CoordConfig, ENV_CF_ACCESS_AUD, ENV_CF_ACCESS_TEAM_DOMAIN, HostPlatform, MapEnv,
    load_coord_config,
};

const LINUX_HOME: &str = "/home/operator";
const TEAM: &str = "team.cloudflareaccess.com";

/// A well-formed audience tag. Built per call because `String::repeat` is not a
/// `const fn`, and a value shared by pointer across tests would let one test's
/// setup leak into another's assertion.
fn well_formed_aud() -> String {
    "a".repeat(64)
}

fn config(env: MapEnv) -> CoordConfig {
    load_coord_config(&env, HostPlatform::Linux).unwrap_or_else(|error| panic!("{error}"))
}

fn refused(env: MapEnv) -> String {
    load_coord_config(&env, HostPlatform::Linux)
        .expect_err("a rejected configuration was accepted")
        .reason
}

#[test]
fn a_blank_cloudflare_access_declaration_is_absence_rather_than_a_refusal() {
    // The shape that produced this: a unit generated from a template, and an
    // operator's `export FOO=`, both leave the variable present and empty. A
    // coordinator with no Cloudflare Access in front of it has to boot on that,
    // because the definition is not wrong — nothing is declared.
    let env = MapEnv::new()
        .with("HOME", LINUX_HOME)
        .with(ENV_CF_ACCESS_TEAM_DOMAIN, "")
        .with(ENV_CF_ACCESS_AUD, "");
    let loaded = config(env);
    assert_eq!(loaded.cf_access_team_domain, None);
    assert_eq!(loaded.cf_access_aud, None);
}

#[test]
fn a_blank_pair_declares_no_access_while_a_real_one_is_kept_whole() {
    // The filter reads empties as absent; it must not touch a declaration that
    // is actually there, or a deployment behind Access would silently stop
    // validating tokens and start issuing them to anyone.
    let env = MapEnv::new()
        .with("HOME", LINUX_HOME)
        .with(ENV_CF_ACCESS_TEAM_DOMAIN, TEAM)
        .with(ENV_CF_ACCESS_AUD, &well_formed_aud());
    let loaded = config(env);
    assert_eq!(loaded.cf_access_team_domain.as_deref(), Some(TEAM));
    assert_eq!(
        loaded.cf_access_aud.as_deref(),
        Some(well_formed_aud().as_str())
    );
}

#[test]
fn a_present_but_malformed_audience_is_still_refused() {
    // This is the assertion that fails if the filter was ever written as "skip
    // validation" instead of "skip empties": the value is present, so the shape
    // check still owns it. The right length, the wrong alphabet, and something
    // that is not a tag at all.
    let malformed = [
        "A".repeat(64),
        "a".repeat(63),
        "z".repeat(64),
        "not-hex-at-all".to_string(),
    ];
    for tag in &malformed {
        let env = MapEnv::new()
            .with("HOME", LINUX_HOME)
            .with(ENV_CF_ACCESS_TEAM_DOMAIN, TEAM)
            .with(ENV_CF_ACCESS_AUD, tag.as_str());
        assert_eq!(
            refused(env),
            "must be 64 lowercase hex characters",
            "{tag:?} was accepted as an audience tag"
        );
    }
}

#[test]
fn a_present_but_malformed_team_domain_is_still_refused() {
    // A label that reaches a different zone, a doubled suffix, and a name the
    // validator must not case-fold. The empty string is deliberately absent
    // from this list: it is not malformed, it is undeclared, and the test above
    // says so.
    for domain in [
        "team.cloudflareaccess.com.evil.test",
        "team.cloudflareaccess.com.cloudflareaccess.com",
        "Team.cloudflareaccess.com",
        "team name.cloudflareaccess.com",
    ] {
        let env = MapEnv::new()
            .with("HOME", LINUX_HOME)
            .with(ENV_CF_ACCESS_TEAM_DOMAIN, domain)
            .with(ENV_CF_ACCESS_AUD, &well_formed_aud());
        assert_eq!(
            refused(env),
            "must be one lowercase label under .cloudflareaccess.com",
            "{domain:?} was accepted as a team domain"
        );
    }
}
