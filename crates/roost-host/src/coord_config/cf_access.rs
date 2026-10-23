//! The shape checks for the Cloudflare Access team domain and audience tag.
//! Owned by `coord_config`, which calls them from `CoordConfig::parse`; they
//! depend only on `roost_protocol` errors and have no environment access.

use roost_protocol::{ProtocolError, ProtocolResult};

/// The Cloudflare Access host suffix a team domain must end in.
const CF_ACCESS_TEAM_DOMAIN_SUFFIX: &str = ".cloudflareaccess.com";

/// The length of a hex-encoded Cloudflare Access application audience tag.
const CF_ACCESS_AUD_LEN: usize = 64;

/// A Cloudflare Access team domain is one label, lowercase, under the Access
/// suffix. The trailing-newline case a JavaScript `$` anchor accepts is
/// refused here rather than reproduced.
pub(super) fn validate_cf_access_team_domain(team_domain: &str) -> ProtocolResult<()> {
    let invalid = || {
        ProtocolError::new(
            "config.cf_access_team_domain",
            format!("must be one lowercase label under {CF_ACCESS_TEAM_DOMAIN_SUFFIX}"),
        )
    };
    let Some(team) = team_domain.strip_suffix(CF_ACCESS_TEAM_DOMAIN_SUFFIX) else {
        return Err(invalid());
    };
    if team.is_empty() {
        return Err(invalid());
    }
    if !team
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid());
    }
    Ok(())
}

/// A Cloudflare Access audience tag is 64 lowercase hex characters.
pub(super) fn validate_cf_access_aud(aud: &str) -> ProtocolResult<()> {
    let valid = aud.len() == CF_ACCESS_AUD_LEN
        && aud
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        return Ok(());
    }
    Err(ProtocolError::new(
        "config.cf_access_aud",
        format!("must be {CF_ACCESS_AUD_LEN} lowercase hex characters"),
    ))
}
