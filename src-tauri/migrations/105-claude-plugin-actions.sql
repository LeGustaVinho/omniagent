CREATE TABLE claude_user_plugin_actions (
    id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('ready', 'applying', 'settled')),
    before_inventory_id TEXT NOT NULL REFERENCES plugin_inventories(id),
    after_inventory_id TEXT REFERENCES plugin_inventories(id),
    preview BLOB NOT NULL,
    outcome BLOB,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    CHECK ((state = 'ready') = (outcome IS NULL))
) STRICT, WITHOUT ROWID;
