-- Pilotwave-owned table. This DDL is mirrored by the Django migration in
-- python/pilotwave/migrations/0001_initial.py; both must stay in sync and the
-- Python round-trip test asserts the columns the worker actually uses.

CREATE TABLE IF NOT EXISTS pilotwave_heartbeat (
    key        varchar(100) NOT NULL PRIMARY KEY,
    last_seen  timestamp with time zone NOT NULL,
    metadata   jsonb        NOT NULL DEFAULT '{}'::jsonb
);

-- The worker writes exactly one row. `last_seen` is always set by the database
-- (NOW()), never by the worker clock:
--
--   INSERT INTO pilotwave_heartbeat (key, last_seen, metadata)
--   VALUES ('pilotwave_worker', NOW(), $1::jsonb)
--   ON CONFLICT (key) DO UPDATE
--   SET last_seen = NOW(), metadata = EXCLUDED.metadata;
