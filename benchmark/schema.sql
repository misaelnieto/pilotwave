-- Benchmark-only scaffolding: creates the two tables the workers touch.
--
-- This is NOT a migration path: development/production schema still comes
-- from Django migrations (`just migrate`). The benchmark applies this file
-- because it talks raw SQL (no Django) and must run on the compose-pinned
-- postgres:14, which the repo's Django 6.x no longer supports for
-- `migrate` (Django 6 requires PostgreSQL 15+).
--
-- Column-for-column compatible with:
--   contracts/sql/001-django-tasks-db-0.13.sql  (queue table mirror)
--   contracts/sql/002-pilotwave-heartbeat.sql  (owned heartbeat table)

CREATE TABLE IF NOT EXISTS django_tasks_database_dbtaskresult (
    id            uuid                  NOT NULL PRIMARY KEY,
    status        varchar(20)           NOT NULL,
    task_path     text                  NOT NULL,
    args_kwargs   jsonb                 NOT NULL,
    priority      integer               NOT NULL,
    run_after     timestamp with time zone NOT NULL,
    queue_name    varchar(32)           NOT NULL,
    backend_name  varchar(32)           NOT NULL,
    enqueued_at   timestamp with time zone NOT NULL,
    started_at    timestamp with time zone NULL,
    finished_at   timestamp with time zone NULL,
    worker_ids    jsonb                 NOT NULL,
    return_value  jsonb                 NULL,
    exception_class_path text           NOT NULL,
    traceback     text                  NOT NULL,

    CONSTRAINT django_tasks_db_status_valid
        CHECK (status IN ('READY', 'RUNNING', 'SUCCESSFUL', 'FAILED')),
    CONSTRAINT django_tasks_db_priority_range
        CHECK (priority BETWEEN -100 AND 100)
);

CREATE TABLE IF NOT EXISTS pilotwave_heartbeat (
    key        varchar(100) NOT NULL PRIMARY KEY,
    last_seen  timestamp with time zone NOT NULL,
    metadata   jsonb        NOT NULL DEFAULT '{}'::jsonb
);
