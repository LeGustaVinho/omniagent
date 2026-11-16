CREATE TABLE project_branch_strategies (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    strategy TEXT NOT NULL CHECK (strategy IN ('independent', 'shared_branch_per_task'))
) STRICT, WITHOUT ROWID;

CREATE TABLE task_branch_strategies (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    override_strategy TEXT CHECK (override_strategy IN ('independent', 'shared_branch_per_task')),
    provisioned_strategy TEXT CHECK (provisioned_strategy IN ('independent', 'shared_branch_per_task')),
    PRIMARY KEY (project_id, task_id),
    FOREIGN KEY (project_id, task_id) REFERENCES project_tasks(project_id, id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- Existing owners and explicit scope assignments keep their recorded checkouts.
-- Pin their hierarchy to the previous default for scopes without an assignment.
INSERT INTO task_branch_strategies(project_id, task_id, provisioned_strategy)
SELECT DISTINCT w.project_id, COALESCE(m.parent_task_id, w.task_id), 'independent'
FROM (
    SELECT project_id, task_id FROM managed_task_worktrees
    UNION
    SELECT project_id, task_id FROM worktree_provisions
    UNION
    SELECT project_id, scope_task_id FROM shared_worktree_scopes
) w
LEFT JOIN project_task_memberships m ON m.project_id = w.project_id AND m.task_id = w.task_id;

ALTER TABLE pending_worktree_changes ADD COLUMN discard_starting_head TEXT;
