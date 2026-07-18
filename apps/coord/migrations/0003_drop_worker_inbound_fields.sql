-- phase-25e: drop dead worker inbound fields.
--
-- reachable_addr / ssh_port / ws_listen_port / ws_scheme were used by
-- the deleted browser↔worker direct WSS path (workers.connect mint +
-- ws-server.ts inbound surface). Both were retired in phase-24c/d.
-- Columns persist in the workers table as NOT NULL fields, blocking
-- new worker registrations once `apps/shared/src/wire/worker.ts`
-- stops emitting them.
--
-- SQLite 3.35+ supports DROP COLUMN; bun:sqlite ships 3.51.

ALTER TABLE workers DROP COLUMN reachable_addr;
ALTER TABLE workers DROP COLUMN ssh_port;
ALTER TABLE workers DROP COLUMN ws_listen_port;
ALTER TABLE workers DROP COLUMN ws_scheme;
