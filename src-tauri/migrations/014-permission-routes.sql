ALTER TABLE agent_permissions ADD COLUMN route TEXT NOT NULL DEFAULT 'native'
    CHECK (route IN ('native', 'claude_prompt'));
