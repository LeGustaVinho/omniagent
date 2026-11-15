ALTER TABLE conversations ADD COLUMN background INTEGER NOT NULL DEFAULT 0 CHECK (background IN (0, 1));
ALTER TABLE jobs ADD COLUMN operator_scope BLOB;
ALTER TABLE job_attempts ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1));

CREATE TABLE job_queue_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    capacity INTEGER NOT NULL CHECK (capacity > 0)
) STRICT, WITHOUT ROWID;
INSERT INTO job_queue_settings VALUES (1, 0, 3);

CREATE TABLE job_approvals (
    id TEXT NOT NULL UNIQUE,
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    decision TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'denied'))
) STRICT, WITHOUT ROWID;
INSERT INTO job_approvals SELECT lower(hex(randomblob(16))), id, 'pending'
FROM jobs WHERE origin_run_id IS NOT NULL OR mode = 'approved_write';

CREATE TABLE job_writer_reservations (
    checkout_id TEXT PRIMARY KEY REFERENCES checkouts(id),
    run_id TEXT NOT NULL UNIQUE REFERENCES job_attempts(run_id)
) STRICT, WITHOUT ROWID;
