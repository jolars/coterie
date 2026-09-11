-- Preserve historical runtime policy and its portable fingerprint on upgrade.
DROP TRIGGER configuration_snapshots_are_append_only;

UPDATE configuration_snapshots
SET document_json = json_set(
    document_json,
    '$.effective.supervision.idle_timeout_seconds', 0,
    '$.provenance."supervision.idle_timeout_seconds"', json_object(
        'source', json_object(
            'layer', 'compiled', 'file', NULL, 'field', 'supervision.idle_timeout_seconds'
        ),
        'selected_by', NULL
    )
)
WHERE scope = 'run'
  AND json_type(document_json, '$.effective.supervision.idle_timeout_seconds') IS NULL;

CREATE TRIGGER configuration_snapshots_are_append_only
BEFORE UPDATE ON configuration_snapshots
BEGIN
    SELECT RAISE(ABORT, 'configuration snapshots are append-only');
END;
