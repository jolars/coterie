-- Older sessions retain unknown process identity; a stored PID is not proof.
CREATE TABLE foreground_process_identity (
    run_id TEXT NOT NULL,
    session_id TEXT PRIMARY KEY,
    identity_json TEXT NOT NULL CHECK (json_valid(identity_json)),
    FOREIGN KEY (run_id, session_id) REFERENCES sessions (run_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TRIGGER foreground_process_identity_immutable
BEFORE UPDATE ON foreground_process_identity
BEGIN
    SELECT RAISE(ABORT, 'foreground process identity is immutable');
END;
