CREATE TABLE checkout_customizations (
    checkout_id TEXT PRIMARY KEY REFERENCES checkouts(id),
    cwd TEXT NOT NULL,
    payload BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE customization_approvals (
    checkout_id TEXT NOT NULL REFERENCES checkouts(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    entry_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    payload BLOB NOT NULL,
    approved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (checkout_id, entry_id, fingerprint)
) STRICT, WITHOUT ROWID;
