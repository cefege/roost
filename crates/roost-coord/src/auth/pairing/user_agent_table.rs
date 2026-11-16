//! The user-agent compatibility table: the ordered browser and operating
//! system names a pairing request is described by.
//!
//! Owned by the pairing slice. Split out of `provenance` because the table is
//! DATA whose order is a contract, and data whose order matters reads better
//! beside the capture that consumes it than buried in it.
//!
//! THE ORDER IS THE CONTRACT, NOT AN IMPLEMENTATION DETAIL. `Edg/` also
//! matches a Chrome user agent and `Chrome/` also matches a Safari one, so a
//! browser listed late would be reported as the one listed early. A table that
//! is ever re-sorted is a table that changes what an operator is told.

use super::provenance::ClientDeviceType;

/// What a user agent says, as a table lookup with no allocation.
///
/// Every entry is a literal substring matched case-sensitively, exactly as the
/// source's case-sensitive patterns do. The ORDER is the contract: `Edg/` also
/// matches a Chrome user agent and `Chrome/` also matches a Safari one, so a
/// browser listed late would otherwise be reported as the one listed early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserAgentDescription {
    /// The browser name, or `None` when the agent names none this table knows.
    pub browser: Option<&'static str>,
    /// The operating system, or `None`.
    pub os: Option<&'static str>,
    /// The device class. A NAMED browser with no mobile signal is a desktop,
    /// which is the one inference in this module; an unknown agent is `None`
    /// rather than a guess, because "unknown" is the fact.
    pub device_type: Option<ClientDeviceType>,
}

/// Describe a user agent using the ordered compatibility table.
#[must_use]
pub fn describe_user_agent(user_agent: &str) -> UserAgentDescription {
    let browser = first_containing(
        user_agent,
        [
            ("Edg/", "Edge"),
            ("OPR/", "Opera"),
            ("Firefox/", "Firefox"),
            ("Chrome/", "Chrome"),
            ("Safari/", "Safari"),
        ],
    );
    let os = first_containing(
        user_agent,
        [
            ("Windows NT", "Windows"),
            ("iPhone", "iOS"),
            ("iPad", "iOS"),
            ("Mac OS X", "macOS"),
            ("Android", "Android"),
            ("Linux", "Linux"),
        ],
    );
    UserAgentDescription {
        browser,
        os,
        device_type: device_type_of(user_agent, browser.is_some()),
    }
}

/// The device class, checking tablet before mobile and defaulting a named
/// browser to desktop.
fn device_type_of(user_agent: &str, named_browser: bool) -> Option<ClientDeviceType> {
    if user_agent.contains("iPad") || user_agent.contains("Tablet") {
        return Some(ClientDeviceType::Tablet);
    }
    if user_agent.contains("Mobile")
        || user_agent.contains("iPhone")
        || user_agent.contains("Android")
    {
        return Some(ClientDeviceType::Mobile);
    }
    named_browser.then_some(ClientDeviceType::Desktop)
}

/// The first table entry whose needle appears in `input`, case-sensitively.
fn first_containing<'a>(
    input: &str,
    table: impl IntoIterator<Item = (&'a str, &'static str)>,
) -> Option<&'static str> {
    table
        .into_iter()
        .find(|(needle, _)| input.contains(needle))
        .map(|(_, name)| name)
}

/// The first table entry whose needle appears in `input`, ignoring case.
///
/// CRATE-VISIBLE, and the other caller is `provenance`'s `sec-ch-ua` hint
/// reader, which is the same question with a different needle set. It lives
/// here rather than being copied because a case-insensitive matcher written
/// twice is free to drift, and the drift is invisible until a browser's
/// `sec-ch-ua` stops matching a table that still looks right.
pub(crate) fn first_case_insensitive<'a>(
    input: &str,
    table: impl IntoIterator<Item = (&'a str, &'static str)>,
) -> Option<&'static str> {
    let lowered = input.to_lowercase();
    table
        .into_iter()
        .find(|(needle, _)| lowered.contains(&needle.to_lowercase()))
        .map(|(_, name)| name)
}
