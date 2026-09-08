ALTER TABLE workspaces
ADD COLUMN generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0);

UPDATE workspaces
SET generation = (
    SELECT generation FROM assignments
    WHERE assignments.id = workspaces.assignment_id
      AND assignments.run_id = workspaces.run_id
);

-- Old integration intents retain the generation that originally owned the work.
UPDATE operations
SET result_json = json_set(
    result_json,
    '$.run_id', run_id,
    '$.generation', (
        SELECT generation FROM assignments
        WHERE assignments.id = json_extract(operations.result_json, '$.assignment_id')
          AND assignments.run_id = operations.run_id
    )
)
WHERE kind = 'workspace.integrate' AND result_json IS NOT NULL;

CREATE TRIGGER assignment_ownership_is_immutable
BEFORE UPDATE OF id, run_id, task_id, agent_id, generation, claim_id ON assignments
WHEN NEW.id <> OLD.id OR NEW.run_id <> OLD.run_id OR NEW.task_id <> OLD.task_id
  OR NEW.agent_id <> OLD.agent_id OR NEW.generation <> OLD.generation
  OR NEW.claim_id <> OLD.claim_id
BEGIN
    SELECT RAISE(ABORT, 'assignment ownership is immutable');
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
