-- D-4b: at-least-once delivery from worker → coord.
--
-- The worker stamps every outbound SessionEvent with a per-worker
-- monotonic client_seq, persisted across worker restarts. Coord
-- inserts under a unique constraint on (worker_fp, client_seq); a
-- retry from the worker after a transient failure conflicts on the
-- index and skips re-insertion. Coord acks every successful insert OR
-- successful dedup so the worker drops the event from its unacked
-- outbox.
--
-- Nullable + partial unique index so events from non-worker producers
-- (synthetic ghost closes, deploy lines) don't need a client_seq.

ALTER TABLE events ADD COLUMN client_seq INTEGER;

CREATE UNIQUE INDEX IF NOT EXISTS events_worker_client_seq
    ON events(worker_fp, client_seq)
    WHERE worker_fp IS NOT NULL AND client_seq IS NOT NULL;
