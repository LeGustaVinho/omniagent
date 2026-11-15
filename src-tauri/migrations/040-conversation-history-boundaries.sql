ALTER TABLE conversations ADD COLUMN history_generation INTEGER NOT NULL DEFAULT 0 CHECK(history_generation >= 0);
ALTER TABLE conversation_contexts ADD COLUMN history_generation INTEGER NOT NULL DEFAULT 0 CHECK(history_generation >= 0);
