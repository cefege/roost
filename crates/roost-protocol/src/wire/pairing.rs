//! How a pairing requester is named to a human: the one label formatter the
//! client's pairing cards and notices and the coordinator's pairing push share.
//!
//! Called by `roost-client-core` (`PairRequest`, `PairedBrowser`) and
//! `roost-coord` (`push::pair_request`). Depends on nothing.

/// What a browser neither the edge nor the requester named is called.
const UNKNOWN_BROWSER_LABEL: &str = "Unknown browser";

/// "Chrome on macOS · Berlin": parsed browser and OS, else the requester's
/// label, else "Unknown browser"; then the most specific known place.
#[must_use]
pub fn requester_label(
    label: &str,
    client_browser: &str,
    client_os: &str,
    city: &str,
    region: &str,
    country_code: &str,
) -> String {
    let browser = client_browser.trim();
    let os = client_os.trim();
    let device = match (browser.is_empty(), os.is_empty()) {
        (false, false) => format!("{browser} on {os}"),
        (false, true) => browser.to_owned(),
        (true, false) => os.to_owned(),
        (true, true) => match label.trim() {
            "" => UNKNOWN_BROWSER_LABEL.to_owned(),
            label => label.to_owned(),
        },
    };
    let place = [city, region, country_code]
        .into_iter()
        .map(str::trim)
        .find(|part| !part.is_empty());
    match place {
        Some(place) => format!("{device} · {place}"),
        None => device,
    }
}
