ALTER TABLE sessions
ADD COLUMN provider_session_id TEXT;

ALTER TABLE sessions
ADD COLUMN reconciliation_state TEXT NOT NULL DEFAULT 'unknown'
    CHECK (reconciliation_state IN ('desired', 'observed', 'lost', 'unknown'));

ALTER TABLE sessions
ADD COLUMN reconciled_at INTEGER;

UPDATE sessions
SET reconciliation_state = CASE
        WHEN state = 'lost' THEN 'lost'
        WHEN state IN ('exited', 'quarantined') THEN 'observed'
        ELSE 'unknown'
    END,
    reconciled_at = COALESCE(ended_at, created_at);

ALTER TABLE workspaces
ADD COLUMN reconciled_at INTEGER;

UPDATE workspaces
SET state = 'unknown'
WHERE state NOT IN ('desired', 'observed', 'lost', 'unknown');

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
