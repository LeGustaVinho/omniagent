CREATE TABLE generated_image_qualifications (
    id TEXT PRIMARY KEY,
    event_seq INTEGER NOT NULL UNIQUE REFERENCES generated_image_evidence_payloads(event_seq) ON DELETE CASCADE,
    qualification_key TEXT NOT NULL,
    checked_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX generated_image_qualifications_scope
    ON generated_image_qualifications(qualification_key, checked_at DESC, event_seq DESC);
