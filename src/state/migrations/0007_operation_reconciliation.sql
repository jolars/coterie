ALTER TABLE operations
ADD COLUMN reconciliation_state TEXT
    CHECK (
        reconciliation_state IS NULL
        OR reconciliation_state IN ('desired', 'observed', 'lost', 'unknown')
    );

ALTER TABLE operations
ADD COLUMN reconciliation_attempt_count INTEGER NOT NULL DEFAULT 0
    CHECK (reconciliation_attempt_count >= 0);

ALTER TABLE operations
ADD COLUMN reconciliation_error TEXT;

ALTER TABLE operations
ADD COLUMN reconciled_at INTEGER;

UPDATE operations
SET reconciliation_state = 'unknown'
WHERE kind IN ('agent.launch_foreground', 'agent.spawn', 'workspace.integrate');

CREATE INDEX operations_reconciliation
ON operations (run_id, reconciliation_state, created_at)
WHERE reconciliation_state IS NOT NULL;
