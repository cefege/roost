-- Authenticated KeeperRuntimeObservationV1 from the latest worker heartbeat.
-- NULL is deliberately fail-closed: pre-reconciliation and failed probes erase
-- stale proof rather than letting rollout admission trust a prior process.

ALTER TABLE workers ADD COLUMN keeper_runtime_json TEXT;
