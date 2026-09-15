-- Historical recoveries deliberately have no invented Git observation.
CREATE TABLE recovery_handoffs (
    assignment_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    operation_id TEXT NOT NULL UNIQUE,
    document_json TEXT NOT NULL CHECK(json_valid(document_json)),
    FOREIGN KEY (run_id, assignment_id) REFERENCES assignments(run_id, id),
    FOREIGN KEY (run_id, operation_id) REFERENCES operations(run_id, id)
) STRICT;

CREATE TRIGGER recovery_handoffs_immutable_update
BEFORE UPDATE ON recovery_handoffs BEGIN
    SELECT RAISE(ABORT, 'recovery handoffs are immutable');
END;

CREATE TRIGGER recovery_handoffs_immutable_delete
BEFORE DELETE ON recovery_handoffs BEGIN
    SELECT RAISE(ABORT, 'recovery handoffs are immutable');
END;
