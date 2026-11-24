-- REFERENCE MIRROR — NOT A MIGRATION APPLIED BY PILOTWAVE
--
-- Mirrors the schema of `django-tasks-db==0.13.0` as verified against a real
-- migrated database. The table is
-- created and migrated by Django via the `django_tasks_db` app; Pilotwave
-- never applies this file. It exists so the Rust worker's SQL and the Python
-- round-trip test can be reviewed against one visible schema.
--
-- Version pin: django-tasks-db==0.13.0
-- Source of truth: django_tasks_db/models.py at that exact version.
-- Known drift from earlier drafts:
--   * table is `django_tasks_database_dbtaskresult` (AppConfig.label is
--     `django_tasks_database`), NOT `django_tasks_db_dbtaskresult`;
--   * `metadata` was removed in migration 0019 — not a column;
--   * `status` literals are READY/RUNNING/SUCCESSFUL/FAILED;
--   * `task_path` is TEXT, not VARCHAR(255);
--   * `run_after` is NOT NULL with a 9999-01-01 sentinel for "no deferral".

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

-- Indexes are part of the reference contract only insofar as the ORM expects
-- them; the worker relies on the claim statement's own ordering + SKIP LOCKED.
-- Names below follow Django's automatic naming and may differ across versions.
CREATE INDEX IF NOT EXISTS dbtaskresult_queue_status_idx
    ON django_tasks_database_dbtaskresult (queue_name, backend_name, status);
CREATE INDEX IF NOT EXISTS dbtaskresult_enqueued_at_idx
    ON django_tasks_database_dbtaskresult (enqueued_at DESC);
