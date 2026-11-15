CREATE TABLE capability_evidence (
    sequence INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    version INTEGER NOT NULL,
    qualification_key TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('protocol_fixture', 'authenticated_fixture')),
    checked_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload BLOB NOT NULL
) STRICT;

CREATE INDEX capability_evidence_matching
    ON capability_evidence(version, qualification_key, origin, sequence DESC);
