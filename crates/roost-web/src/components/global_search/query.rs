//! `/search`'s three route parameters, read out of the address bar and written
//! back into it.
//!
//! The search page has no form state of its own: `q`, `scope` and `case` ARE the
//! route, so a result list a reader shares or returns to is the same list. That
//! is why this is a codec over the location rather than three signals in the
//! component — the component reads what this parses and writes what this
//! serializes, and the URL is the only place either of them lives.
//!
//! Ports the parameters of `apps/web/src/components/search/GlobalSearchPage.tsx:35-47`
//! and its `updateRoute` at `:69-80`.
use roost_client_core::client::global_search::GlobalSearchQuery;

use crate::routes::{percent_decode, percent_encode};

/// Which rows the page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchScope {
    /// Every session, plus retained terminal content.
    #[default]
    All,
    /// Only the sessions whose agent wants attention. Content search is off:
    /// a filter that answers "what needs me" is not a fleet-wide text scan.
    Attention,
}

/// What `/search` is currently asking for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchRouteQuery {
    /// Which rows are listed.
    pub scope: SearchScope,
    /// The literal typed into the field.
    pub text: String,
    /// Whether a match respects case.
    pub case_sensitive: bool,
}

impl SearchRouteQuery {
    /// Read the parameters out of `location`, which is a path and its query.
    ///
    /// An unknown or repeated parameter takes the first value and is otherwise
    /// ignored: the grammar owns which parameters exist, and a browser that
    /// appended one must not change what the page means.
    #[must_use]
    pub fn parse(location: &str) -> Self {
        let Some(query) = location.split_once('?').map(|(_, query)| query) else {
            return Self::default();
        };
        let mut parsed = Self::default();
        for parameter in query.split('&') {
            let (key, value) = parameter.split_once('=').unwrap_or((parameter, ""));
            let value = percent_decode(value).unwrap_or_else(|| value.to_owned());
            match key {
                "q" if parsed.text.is_empty() => parsed.text = value,
                "scope" => {
                    parsed.scope = if value == "attention" {
                        SearchScope::Attention
                    } else {
                        SearchScope::All
                    };
                }
                "case" => parsed.case_sensitive = value == "1",
                _ => {}
            }
        }
        parsed
    }

    /// The content search this route asks the coordinator for.
    ///
    /// The attention scope clears it rather than sending nothing: a controller
    /// left holding the previous query would keep publishing that query's rows
    /// under a list that is now saying something else.
    #[must_use]
    pub fn content_query(&self) -> GlobalSearchQuery {
        match self.scope {
            SearchScope::All => GlobalSearchQuery {
                query: self.text.clone(),
                case_sensitive: self.case_sensitive,
            },
            SearchScope::Attention => GlobalSearchQuery::default(),
        }
    }

    /// The address this query is written as, in the order the page emits them.
    ///
    /// Only non-default parameters are written, so `/search` with nothing typed
    /// is a clean URL and a reader's Back button does not walk through empty
    /// states they never visited.
    #[must_use]
    pub fn to_path(&self) -> String {
        let mut parameters: Vec<String> = Vec::new();
        if self.scope == SearchScope::Attention {
            parameters.push("scope=attention".to_owned());
        }
        if !self.text.trim().is_empty() {
            parameters.push(format!("q={}", percent_encode(self.text.trim())));
        }
        if self.scope == SearchScope::All && self.case_sensitive {
            parameters.push("case=1".to_owned());
        }
        if parameters.is_empty() {
            "/search".to_owned()
        } else {
            format!("/search?{}", parameters.join("&"))
        }
    }

    /// This query with the literal replaced, for one keystroke.
    #[must_use]
    pub fn with_text(&self, text: &str) -> Self {
        Self {
            text: text.to_owned(),
            ..self.clone()
        }
    }

    /// The one-line sentence under the field, which is also its label.
    #[must_use]
    pub fn search_label(&self) -> &'static str {
        match self.scope {
            SearchScope::All => "Search sessions and terminal content",
            SearchScope::Attention => "Filter attention",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_route_is_the_default_query() {
        assert_eq!(
            SearchRouteQuery::parse("/search"),
            SearchRouteQuery::default()
        );
        assert_eq!(
            SearchRouteQuery::parse("/search?"),
            SearchRouteQuery::default()
        );
    }

    #[test]
    fn the_query_survives_a_round_trip_through_the_address_bar() {
        let parsed = SearchRouteQuery::parse("/search?q=metadata-404ef456&case=1");
        assert_eq!(parsed.text, "metadata-404ef456");
        assert!(parsed.case_sensitive);
        assert_eq!(parsed.to_path(), "/search?q=metadata-404ef456&case=1");
    }

    #[test]
    fn a_value_that_would_split_a_parameter_is_escaped_and_read_back() {
        let query = SearchRouteQuery::default().with_text("a&b=c d");
        assert_eq!(query.to_path(), "/search?q=a%26b%3Dc%20d");
        assert_eq!(SearchRouteQuery::parse(&query.to_path()).text, "a&b=c d");
    }

    #[test]
    fn the_attention_scope_searches_no_content() {
        let query = SearchRouteQuery {
            scope: SearchScope::Attention,
            text: "blocked".to_owned(),
            case_sensitive: true,
        };
        assert!(query.content_query().is_empty());
        assert_eq!(query.to_path(), "/search?scope=attention&q=blocked");
    }
}
