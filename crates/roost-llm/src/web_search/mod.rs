//! Web search engines and provider-hosted search for agent tools.
//! Callers provide a parsed query once and receive normalized sources and citations.
//! HTTP is injected so tests, proxies and cancellation share the provider clients.

mod engines;
mod hosted;
mod public;
mod query;
mod response;
mod scraper;
mod scraper_results;
mod types;

pub use engines::{ENGINE_IDS, engine_label, engine_needs_key, search_engine};
pub use hosted::hosted_search;
pub use query::{ParsedQuery, apply_constraints, parse_query};
pub use types::{Citation, Recency, SearchQuery, SearchResponse, SearchSource, format_for_model};
