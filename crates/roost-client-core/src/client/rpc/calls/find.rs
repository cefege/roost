//! `SessionsSearchScrollback` and `SessionsCancelScrollbackSearch`: one page of a
//! pane's retained history, and the cancel of a scan already running.
//!
//! Called by roost-web's pane mount through `CoordRpc::call`, one page at a time
//! under the `search_id` a search owns. v2's equivalent is the
//! `coordClient.sessionsSearchScrollback` call inside
//! `apps/web/src/client/search/terminalFindPageChain.ts`; the cancellation is v2's
//! `coordClient.sessionsCancelScrollbackSearch` in
//! `apps/web/src/renderer/terminalFindController.ts:117`.
//!
//! The page is returned UNVALIDATED. Judging it — is this the window the reader
//! asked for, does every match sit inside it, does the cursor move the right way
//! — is `roost_client_core::search`, and it happens in the find controller that
//! asked, so a page can be refused against the search that issued it.

use roost_proto::{
    SearchStopReason as PbStopReason, SessionsCancelScrollbackSearchRequest,
    SessionsCancelScrollbackSearchResponse, SessionsSearchScrollbackMatch,
    SessionsSearchScrollbackRequest, SessionsSearchScrollbackResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;
use crate::search::{RawMatch, SearchPage, SearchStop};

/// `SessionsSearchScrollback`: one page of matches, scanning newest rows first.
///
/// `grid_epoch` is what fences the page: a browser names the numbering of the
/// frame it holds and the worker refuses to answer from any other one, because a
/// row index from a previous epoch lands inside the new epoch's valid range while
/// naming unrelated content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchScrollback {
    /// The session whose history is read.
    pub session_id: String,
    /// The identity every page of one search echoes, and a cancel names.
    pub search_id: String,
    /// The grid numbering to pin; empty binds to the worker's current epoch.
    pub grid_epoch: String,
    /// The literal the reader typed, or the pattern they toggled regex on.
    pub query: String,
    /// Whether the scan is case-sensitive.
    pub case_sensitive: bool,
    /// Whether `query` is a pattern rather than a literal.
    pub regex: bool,
    /// The most matches this page may return.
    pub max_matches: u32,
    /// The most complete rows this page may scan.
    pub max_rows: u32,
    /// The row this page starts at, exclusive; absent begins at the newest row.
    pub before_row: Option<u64>,
}

/// What `SessionsSearchScrollback` answered, as rows and a window.
///
/// The window and the matches travel side by side rather than nested, because the
/// window is what the guards judge and a match read out of an unvalidated window
/// is a row that points at the wrong line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchScrollbackPage {
    /// The rows the scan found, unfenced to any epoch.
    pub matches: Vec<RawMatch>,
    /// The window the scan read, and where it would resume.
    pub page: SearchPage,
    /// The grid numbering those rows belong to.
    pub grid_epoch: String,
    /// Why the scan stopped.
    pub stop: SearchStop,
}

impl UnaryMethod for SearchScrollback {
    const METHOD: &'static str = "SessionsSearchScrollback";
    type Response = SearchScrollbackPage;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsSearchScrollbackRequest {
                session_id: self.session_id.clone(),
                query: self.query.clone(),
                case_sensitive: self.case_sensitive,
                regex: self.regex,
                max_matches: self.max_matches,
                grid_epoch: self.grid_epoch.clone(),
                before_row: self.before_row,
                max_rows: self.max_rows,
                search_id: self.search_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<SearchScrollbackPage, RpcCodecError> {
        let response: SessionsSearchScrollbackResponse = decode_message(Self::METHOD, body)?;
        Ok(SearchScrollbackPage {
            matches: response
                .matches
                .iter()
                .map(raw_match)
                .collect::<Result<Vec<_>, _>>()?,
            page: SearchPage {
                scanned_start_row: row(response.scanned_start_row, "scanned_start_row")?,
                scanned_end_row: row(response.scanned_end_row, "scanned_end_row")?,
                next_before_row: response
                    .next_before_row
                    .map(|next| row(next, "next_before_row"))
                    .transpose()?,
            },
            grid_epoch: response.grid_epoch.clone(),
            stop: stop(response.stop_reason.as_known()),
        })
    }
}

/// `SessionsCancelScrollbackSearch`: stop a scan the coordinator is still running.
///
/// The answer is empty and the only thing that matters is that the call went out:
/// a page already in flight is dropped by the search id it echoes, not by the
/// cancel having landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelScrollbackSearch {
    /// The session whose scan is stopped.
    pub session_id: String,
    /// The search to stop.
    pub search_id: String,
}

impl UnaryMethod for CancelScrollbackSearch {
    const METHOD: &'static str = "SessionsCancelScrollbackSearch";
    type Response = ();

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsCancelScrollbackSearchRequest {
                session_id: self.session_id.clone(),
                search_id: self.search_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(_body: &[u8]) -> Result<(), RpcCodecError> {
        let _: SessionsCancelScrollbackSearchResponse = decode_message(Self::METHOD, _body)?;
        Ok(())
    }
}

/// One match, or a refusal: a row past the client's `u32` row space cannot be
/// represented, and reading it as some other row would point at the wrong line.
fn raw_match(match_row: &SessionsSearchScrollbackMatch) -> Result<RawMatch, RpcCodecError> {
    Ok(RawMatch {
        row: row(match_row.row, "match row")?,
        col: match_row.col,
        len: match_row.len,
        preview: match_row.preview.clone(),
    })
}

fn row(value: u64, field: &'static str) -> Result<u32, RpcCodecError> {
    u32::try_from(value).map_err(|_| RpcCodecError::MalformedResponse {
        method: SearchScrollback::METHOD,
        detail: format!("{field} {value} is outside the client's row space"),
    })
}

/// The reason a scan stopped, as the chain reads the wire enum.
///
/// `Unspecified` is the wire's zero value: its page is read, and then the chain
/// fails, because no reason is not a reason to continue.
fn stop(reason: Option<PbStopReason>) -> SearchStop {
    match reason {
        None | Some(PbStopReason::SEARCH_STOP_REASON_UNSPECIFIED) => SearchStop::Unspecified,
        Some(PbStopReason::SEARCH_STOP_REASON_COMPLETE) => SearchStop::Complete,
        Some(PbStopReason::SEARCH_STOP_REASON_ROW_LIMIT) => SearchStop::RowLimit,
        Some(PbStopReason::SEARCH_STOP_REASON_MATCH_LIMIT) => SearchStop::MatchLimit,
        Some(PbStopReason::SEARCH_STOP_REASON_DEADLINE) => SearchStop::Deadline,
        Some(PbStopReason::SEARCH_STOP_REASON_EPOCH_CHANGED) => SearchStop::EpochChanged,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::window_is_valid;

    /// A page the coordinator could plausibly answer. The stop reason is a
    /// parameter because it is the one field these tests vary.
    fn response(stop_reason: PbStopReason) -> SessionsSearchScrollbackResponse {
        SessionsSearchScrollbackResponse {
            matches: vec![SessionsSearchScrollbackMatch {
                row: 400,
                col: 3,
                len: 6,
                preview: "FINDLINE-400".to_owned(),
                ..Default::default()
            }],
            scrollback_total: 3_000,
            cols: 80,
            grid_epoch: "epoch-1".to_owned(),
            scanned_start_row: 0,
            scanned_end_row: 1_000,
            next_before_row: Some(0),
            stop_reason: stop_reason.into(),
            ..Default::default()
        }
    }

    /// Round-trip a response through the real encoder and the real decoder, so
    /// the test proves what the coordinator's bytes decode to rather than what a
    /// hand-built value decodes to.
    fn round_trip(stop_reason: PbStopReason) -> SearchScrollbackPage {
        let body = encode_message(SearchScrollback::METHOD, &response(stop_reason))
            .expect("a fixture response encodes");
        SearchScrollback::decode_response(&body).expect("a fixture response decodes")
    }

    #[test]
    fn a_page_survives_the_wire_with_the_window_the_guards_judge() {
        // The window is what decides whether a match may be shown, so a decode
        // that lost or shifted it would publish a row from unscanned history as
        // though the coordinator had read it.
        let page = round_trip(PbStopReason::SEARCH_STOP_REASON_COMPLETE);
        assert_eq!(
            page.page,
            SearchPage {
                scanned_start_row: 0,
                scanned_end_row: 1_000,
                next_before_row: Some(0),
            }
        );
        assert_eq!(page.grid_epoch, "epoch-1");
        assert!(window_is_valid(&page.page, &page.matches, None));
    }

    #[test]
    fn every_stop_reason_reaches_the_chain_as_its_own_arm() {
        // The chain branches on the stop reason, so a reason that decoded as
        // `Unspecified` would turn a completed scan into a failed one: the page
        // would be read, found wanting, and reported as an error the reader did
        // not cause.
        for (wire, expected) in [
            (
                PbStopReason::SEARCH_STOP_REASON_UNSPECIFIED,
                SearchStop::Unspecified,
            ),
            (
                PbStopReason::SEARCH_STOP_REASON_COMPLETE,
                SearchStop::Complete,
            ),
            (
                PbStopReason::SEARCH_STOP_REASON_ROW_LIMIT,
                SearchStop::RowLimit,
            ),
            (
                PbStopReason::SEARCH_STOP_REASON_MATCH_LIMIT,
                SearchStop::MatchLimit,
            ),
            (
                PbStopReason::SEARCH_STOP_REASON_DEADLINE,
                SearchStop::Deadline,
            ),
            (
                PbStopReason::SEARCH_STOP_REASON_EPOCH_CHANGED,
                SearchStop::EpochChanged,
            ),
        ] {
            assert_eq!(round_trip(wire).stop, expected, "{wire:?} decoded wrong");
        }
    }

    #[test]
    fn a_match_row_outside_the_clients_row_space_refuses_the_page() {
        // A row that does not fit the client's row space is not a short page, it
        // is a WRONG one: truncating it would highlight a different line, so the
        // decode fails and the chain reports the failure instead.
        let mut over = response(PbStopReason::SEARCH_STOP_REASON_COMPLETE);
        over.matches[0].row = u64::from(u32::MAX) + 1;
        let body = encode_message(SearchScrollback::METHOD, &over).expect("encodes");
        assert!(SearchScrollback::decode_response(&body).is_err());
    }

    #[test]
    fn a_scanned_window_the_coordinator_never_read_is_refused_by_the_guard() {
        // The decode is deliberately unjudged, so this pins that the guard is what
        // refuses: a match outside the window the coordinator actually scanned
        // belongs to a different scan, and the chain must drop it rather than
        // paint it.
        let page = round_trip(PbStopReason::SEARCH_STOP_REASON_COMPLETE);
        let outside = vec![RawMatch {
            row: 5_000,
            col: 0,
            len: 4,
            preview: String::new(),
        }];
        assert!(!window_is_valid(&page.page, &outside, None));
    }
}
