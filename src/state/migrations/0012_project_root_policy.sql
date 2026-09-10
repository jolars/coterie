-- Historical runs granted no agent authority to attach additional project roots.
-- Host paths remain outside the portable fingerprint.
DROP TRIGGER configuration_snapshots_are_append_only;

UPDATE configuration_snapshots
SET document_json = json_set(
    document_json,
    '$.effective.allowed_project_roots', json('[]'),
    '$.provenance.allowed_project_roots', json_object(
        'source', json_object(
            'layer', 'compiled', 'file', NULL, 'field', 'allowed_project_roots'
        ),
        'selected_by', NULL
    )
)
WHERE scope = 'run'
  AND json_type(document_json, '$.effective.allowed_project_roots') IS NULL;

CREATE TRIGGER configuration_snapshots_are_append_only
BEFORE UPDATE ON configuration_snapshots
BEGIN
    SELECT RAISE(ABORT, 'configuration snapshots are append-only');
END;
