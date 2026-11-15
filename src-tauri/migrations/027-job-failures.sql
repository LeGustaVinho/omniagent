CREATE TABLE job_failures (
    run_id TEXT PRIMARY KEY REFERENCES job_attempts(run_id),
    checkout_root TEXT NOT NULL,
    report TEXT CHECK (report IS NULL OR json_valid(report))
) STRICT, WITHOUT ROWID;

INSERT INTO job_failures (run_id, checkout_root)
SELECT a.run_id, c.root FROM job_attempts a
JOIN runs r ON r.id = a.run_id JOIN checkouts c ON c.id = r.checkout_id
WHERE r.status IN ('failed', 'interrupted');
