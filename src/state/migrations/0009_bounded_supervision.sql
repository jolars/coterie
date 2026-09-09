CREATE TABLE run_shutdowns (
    run_id TEXT PRIMARY KEY REFERENCES runs (id) ON DELETE RESTRICT,
    operation_id TEXT NOT NULL UNIQUE,
    phase TEXT NOT NULL CHECK (phase IN ('interrupting', 'terminating', 'reconciling', 'timed_out', 'completed')),
    requested_at_ms INTEGER NOT NULL,
    interrupt_until_ms INTEGER NOT NULL,
    deadline_ms INTEGER NOT NULL,
    CHECK (requested_at_ms <= interrupt_until_ms AND interrupt_until_ms < deadline_ms),
    FOREIGN KEY (run_id, operation_id) REFERENCES operations (run_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE session_controls (
    session_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    reason TEXT NOT NULL CHECK (reason IN ('shutdown', 'startup_timeout', 'execution_timeout')),
    phase TEXT NOT NULL CHECK (phase IN ('interrupt', 'terminate', 'kill', 'timed_out', 'completed')),
    delivered INTEGER NOT NULL DEFAULT 0 CHECK (delivered IN (0, 1)),
    requested_at_ms INTEGER NOT NULL,
    interrupt_until_ms INTEGER NOT NULL,
    kill_at_ms INTEGER NOT NULL,
    deadline_ms INTEGER NOT NULL,
    CHECK (requested_at_ms <= interrupt_until_ms AND interrupt_until_ms <= kill_at_ms AND kill_at_ms < deadline_ms),
    FOREIGN KEY (run_id, agent_id, session_id) REFERENCES sessions (run_id, agent_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE session_launch_attempts (
    session_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    window_started_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    next_attempt_at INTEGER NOT NULL,
    in_flight INTEGER NOT NULL CHECK (in_flight IN (0, 1)),
    quarantined INTEGER NOT NULL DEFAULT 0 CHECK (quarantined IN (0, 1)),
    FOREIGN KEY (run_id, agent_id, session_id) REFERENCES sessions (run_id, agent_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE session_failures (
    session_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    failed_at INTEGER NOT NULL,
    quarantine_until INTEGER,
    FOREIGN KEY (run_id, agent_id, session_id) REFERENCES sessions (run_id, agent_id, id) ON DELETE RESTRICT
) STRICT;

CREATE INDEX session_failure_windows ON session_failures (run_id, agent_id, failed_at);
