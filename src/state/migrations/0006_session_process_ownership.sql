ALTER TABLE sessions
ADD COLUMN process_owner TEXT NOT NULL DEFAULT 'supervisor'
    CHECK (process_owner IN ('supervisor', 'foreground'));

UPDATE sessions
SET process_owner = 'foreground'
WHERE provider_session_id GLOB 'process:[0-9]*';
