CREATE TABLE plugin_inventories (
    seq INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    provider TEXT NOT NULL,
    checked_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload BLOB NOT NULL
) STRICT;

CREATE INDEX plugin_inventories_by_provider
    ON plugin_inventories(provider, seq DESC);
