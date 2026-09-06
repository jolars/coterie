CREATE UNIQUE INDEX session_credential_scope
    ON sessions (run_id, agent_id, id, generation);

CREATE TABLE session_credentials (
    session_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    token_verifier BLOB NOT NULL CHECK (
        typeof(token_verifier) = 'blob' AND length(token_verifier) = 32
    ),
    created_at INTEGER NOT NULL,
    revoked_at INTEGER CHECK (
        revoked_at IS NULL OR revoked_at >= created_at
    ),
    FOREIGN KEY (run_id, agent_id, session_id, generation)
        REFERENCES sessions (run_id, agent_id, id, generation)
        ON DELETE RESTRICT,
    UNIQUE (run_id, agent_id, session_id)
) STRICT;

CREATE UNIQUE INDEX one_active_session_credential_per_agent
    ON session_credentials (run_id, agent_id)
    WHERE revoked_at IS NULL;
