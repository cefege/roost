//! Synchronous capability readings for pointer and motion policies.
//! Reactive component observation uses Libero's use_media_query instead.
//! Missing browser support is distinct from a query that does not match.

pub fn media_query_match(query: &str) -> Option<bool> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()?
            .match_media(query)
            .ok()
            .flatten()
            .map(|query| query.matches())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = query;
        None
    }
}
