CREATE TRIGGER messages_cannot_be_deleted
BEFORE DELETE ON messages
BEGIN
    SELECT RAISE(ABORT, 'messages are durable');
END;

CREATE TRIGGER message_content_is_immutable
BEFORE UPDATE OF id, run_id, sender_agent_id, recipient_agent_id, sequence, body, created_at
ON messages
BEGIN
    SELECT RAISE(ABORT, 'message content is immutable');
END;

CREATE TRIGGER message_acknowledgements_are_final
BEFORE UPDATE OF acknowledged_at ON messages
WHEN OLD.acknowledged_at IS NOT NULL
    OR NEW.acknowledged_at IS NULL
    OR NEW.acknowledged_at < OLD.created_at
BEGIN
    SELECT RAISE(ABORT, 'message acknowledgements are final');
END;
