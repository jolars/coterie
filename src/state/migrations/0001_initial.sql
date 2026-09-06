CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    stopped_at INTEGER
) STRICT;

CREATE TABLE configuration_snapshots (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL,
    project_id TEXT,
    scope TEXT NOT NULL CHECK (scope IN ('run', 'project')),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    fingerprint TEXT NOT NULL,
    document_json TEXT NOT NULL CHECK (json_valid(document_json)),
    created_at INTEGER NOT NULL,
    CHECK (
        (scope = 'run' AND project_id IS NULL)
        OR (scope = 'project' AND project_id IS NOT NULL)
    ),
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, project_id) REFERENCES projects (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE UNIQUE INDEX one_run_configuration_snapshot
    ON configuration_snapshots (run_id)
    WHERE project_id IS NULL;

CREATE UNIQUE INDEX one_project_configuration_snapshot
    ON configuration_snapshots (run_id, project_id)
    WHERE project_id IS NOT NULL;

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    original_path BLOB NOT NULL,
    canonical_path BLOB NOT NULL,
    identity_json TEXT NOT NULL CHECK (json_valid(identity_json)),
    is_primary INTEGER NOT NULL CHECK (is_primary IN (0, 1)),
    attached_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    UNIQUE (run_id, id),
    UNIQUE (run_id, alias),
    UNIQUE (run_id, canonical_path)
) STRICT;

CREATE UNIQUE INDEX one_primary_project_per_run
    ON projects (run_id)
    WHERE is_primary = 1;

CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    role TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    provider TEXT NOT NULL,
    state TEXT NOT NULL,
    transcript_path BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    ended_at INTEGER,
    FOREIGN KEY (run_id, agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    UNIQUE (agent_id, generation),
    UNIQUE (run_id, id),
    UNIQUE (run_id, agent_id, id)
) STRICT;

CREATE TABLE task_groups (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL,
    name TEXT,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    group_id INTEGER,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('open', 'in_progress', 'submitted', 'closed', 'canceled')
    ),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, project_id) REFERENCES projects (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, group_id) REFERENCES task_groups (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE task_dependencies (
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    dependency_task_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, dependency_task_id),
    CHECK (task_id <> dependency_task_id),
    FOREIGN KEY (run_id, task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, dependency_task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE comments (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    author_agent_id TEXT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (run_id, task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, author_agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    actor_agent_id TEXT,
    status TEXT NOT NULL,
    request_json TEXT NOT NULL CHECK (json_valid(request_json)),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    attempt_count INTEGER NOT NULL CHECK (attempt_count >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, actor_agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE claims (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    state TEXT NOT NULL,
    claimed_at INTEGER NOT NULL,
    released_at INTEGER,
    FOREIGN KEY (run_id, task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, operation_id) REFERENCES operations (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE UNIQUE INDEX one_active_claim_per_task
    ON claims (task_id)
    WHERE released_at IS NULL;

CREATE TABLE assignments (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    session_id TEXT,
    claim_id INTEGER NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    state TEXT NOT NULL,
    summary TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    FOREIGN KEY (run_id, task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, agent_id, session_id) REFERENCES sessions (run_id, agent_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, claim_id) REFERENCES claims (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    sender_agent_id TEXT,
    recipient_agent_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    acknowledged_at INTEGER,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, sender_agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, recipient_agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, recipient_agent_id, sequence),
    UNIQUE (run_id, id)
) STRICT;

CREATE TABLE workspaces (
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
    FOREIGN KEY (run_id, assignment_id) REFERENCES assignments (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, project_id) REFERENCES projects (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, path)
) STRICT;

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    subject TEXT NOT NULL,
    project_id TEXT,
    agent_id TEXT,
    task_id TEXT,
    operation_id TEXT,
    correlation_id TEXT,
    causation_id TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, project_id) REFERENCES projects (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, agent_id) REFERENCES agents (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, task_id) REFERENCES tasks (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, operation_id) REFERENCES operations (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, correlation_id) REFERENCES events (run_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (run_id, causation_id) REFERENCES events (run_id, id) ON DELETE RESTRICT,
    UNIQUE (run_id, id),
    UNIQUE (run_id, sequence)
) STRICT;

CREATE TRIGGER configuration_snapshots_are_append_only
BEFORE UPDATE ON configuration_snapshots
BEGIN
    SELECT RAISE(ABORT, 'configuration snapshots are append-only');
END;

CREATE TRIGGER configuration_snapshots_cannot_be_deleted
BEFORE DELETE ON configuration_snapshots
BEGIN
    SELECT RAISE(ABORT, 'configuration snapshots are append-only');
END;

CREATE TRIGGER events_are_append_only
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_cannot_be_deleted
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
