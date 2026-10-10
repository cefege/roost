//! The LLM provider layer of the agent harness: the model catalog, provider wire clients,
//! OAuth logins, the multi-account credential pool, usage reports and the judge.
//! Called by roost-agent and the coordinator; depends on reqwest and roost-observability.

#![forbid(unsafe_code)]
