-- Workers report whether their long-lived keeper subprocess is running stale
-- code (a keeper outlives a worker deploy, so a behavior-only change can stay
-- dormant). Non-null = stale, value = the running keeper's short build stamp;
-- null = current. Set from heartbeat; drives the MachinesPane "keeper stale"
-- badge + `roost keeper-refresh`.
-- nullable: workers registered before the field was added stay valid.

ALTER TABLE workers ADD COLUMN keeper_stale TEXT;
