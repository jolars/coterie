CREATE TABLE foreground_notifications (
    session_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    thread_id TEXT,
    event_cursor INTEGER NOT NULL CHECK (event_cursor >= 0),
    message_cursor INTEGER NOT NULL DEFAULT 0 CHECK (message_cursor >= 0),
    FOREIGN KEY (run_id, agent_id, session_id, generation)
        REFERENCES sessions (run_id, agent_id, id, generation)
) STRICT;

CREATE TRIGGER foreground_notification_destination_immutable
BEFORE UPDATE ON foreground_notifications
WHEN NEW.session_id <> OLD.session_id OR NEW.run_id <> OLD.run_id
    OR NEW.agent_id <> OLD.agent_id OR NEW.generation <> OLD.generation
    OR (OLD.thread_id IS NOT NULL AND NEW.thread_id IS NOT OLD.thread_id)
BEGIN
    SELECT RAISE(ABORT, 'notification destination is immutable');
END;

CREATE TABLE notification_deliveries (
    operation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES foreground_notifications(session_id),
    event_cursor INTEGER NOT NULL,
    message_cursor INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('attempting', 'accepted', 'failed', 'unknown')),
    created_at INTEGER NOT NULL,
    observed_at INTEGER
) STRICT;

CREATE INDEX notification_delivery_session ON notification_deliveries(session_id, state);
