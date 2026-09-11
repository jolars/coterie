-- Project directories are shared locations, while each binding retains its history.
CREATE TABLE workspace_bindings (
    assignment_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    path BLOB NOT NULL,
    state TEXT NOT NULL,
    base_commit TEXT,
    result_commit TEXT,
    target_commit TEXT,
    created_at INTEGER NOT NULL,
    reconciled_at INTEGER,
    generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
    FOREIGN KEY (run_id, assignment_id) REFERENCES assignments (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, project_id) REFERENCES projects (run_id, id) ON DELETE RESTRICT
) STRICT;

INSERT INTO workspace_bindings (
    rowid, assignment_id, run_id, project_id, kind, path, state, base_commit,
    result_commit, target_commit, created_at, reconciled_at, generation
)
SELECT rowid, assignment_id, run_id, project_id, kind, path, state, base_commit,
       result_commit, target_commit, created_at, reconciled_at, generation
FROM workspaces;

DROP TABLE workspaces;
ALTER TABLE workspace_bindings RENAME TO workspaces;

CREATE INDEX workspace_paths ON workspaces (run_id, path);
CREATE UNIQUE INDEX isolated_workspace_paths ON workspaces (run_id, path)
WHERE kind = 'worktree';

CREATE TRIGGER workspace_reconciliation_state_on_insert
BEFORE INSERT ON workspaces
WHEN NEW.state NOT IN ('desired', 'observed', 'lost', 'unknown')
BEGIN
    SELECT RAISE(ABORT, 'invalid workspace reconciliation state');
END;

CREATE TRIGGER workspace_reconciliation_state_on_update
BEFORE UPDATE OF state ON workspaces
WHEN NEW.state NOT IN ('desired', 'observed', 'lost', 'unknown')
BEGIN
    SELECT RAISE(ABORT, 'invalid workspace reconciliation state');
END;

CREATE TRIGGER workspace_generation_on_insert
BEFORE INSERT ON workspaces
WHEN NOT EXISTS (
    SELECT 1 FROM assignments
    JOIN tasks ON tasks.id = assignments.task_id AND tasks.run_id = assignments.run_id
    WHERE assignments.id = NEW.assignment_id AND assignments.run_id = NEW.run_id
      AND assignments.generation = NEW.generation AND tasks.project_id = NEW.project_id
)
BEGIN
    SELECT RAISE(ABORT, 'workspace ownership does not match its assignment');
END;

CREATE TRIGGER workspace_ownership_is_immutable
BEFORE UPDATE OF assignment_id, run_id, project_id, generation, kind, path, base_commit ON workspaces
WHEN NEW.assignment_id <> OLD.assignment_id OR NEW.run_id <> OLD.run_id
  OR NEW.project_id <> OLD.project_id OR NEW.generation <> OLD.generation
  OR NEW.kind <> OLD.kind OR NEW.path <> OLD.path OR NEW.base_commit IS NOT OLD.base_commit
BEGIN
    SELECT RAISE(ABORT, 'workspace ownership is immutable');
END;

-- Submission is not process exit. Uncertain or unassociated sessions retain ownership.
CREATE VIEW workspace_path_ownership AS
SELECT workspace.assignment_id, workspace.run_id, workspace.path,
       workspace.kind NOT IN ('project', 'read_only') AS exclusive,
       workspace.kind = 'project' AND (
           assignment.completed_at IS NULL
           OR assignment.state NOT IN ('completed', 'released', 'canceled')
           OR assignment.session_id IS NULL
           OR NOT EXISTS (
               SELECT 1 FROM sessions
               WHERE sessions.id = assignment.session_id
                 AND sessions.run_id = assignment.run_id
                 AND sessions.agent_id = assignment.agent_id
                 AND sessions.generation = assignment.generation
                 AND sessions.state = 'exited'
                 AND sessions.reconciliation_state = 'observed'
           )
           OR EXISTS (
               SELECT 1 FROM sessions
               WHERE sessions.run_id = assignment.run_id
                 AND sessions.agent_id = assignment.agent_id
                 AND (sessions.state <> 'exited' OR sessions.reconciliation_state <> 'observed')
           )
       ) AS writer
FROM workspaces AS workspace
JOIN assignments AS assignment ON assignment.id = workspace.assignment_id
    AND assignment.run_id = workspace.run_id;

CREATE TRIGGER workspace_path_admission
BEFORE INSERT ON workspaces
WHEN EXISTS (
    SELECT 1 FROM workspace_path_ownership
    WHERE run_id = NEW.run_id AND path = NEW.path
      AND (exclusive OR NEW.kind NOT IN ('project', 'read_only')
           OR (NEW.kind = 'project' AND writer))
)
BEGIN
    SELECT RAISE(ABORT, 'workspace path is reserved by another assignment');
END;
