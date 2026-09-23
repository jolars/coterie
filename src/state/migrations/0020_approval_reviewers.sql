-- Record the reviewer historically enforced by Coterie without changing authority.
DROP TRIGGER configuration_snapshots_are_append_only;

UPDATE configuration_snapshots
SET document_json = json_set(
    document_json,
    '$.effective.archetype.permission_profiles', json((
        SELECT json_group_object(key, json_insert(value, '$.approval_reviewer', 'user'))
        FROM json_each(document_json, '$.effective.archetype.permission_profiles')
    )),
    '$.effective.roles', json((
        SELECT json_group_object(key, json_insert(value, '$.permission_profile.approval_reviewer', 'user'))
        FROM json_each(document_json, '$.effective.roles')
    )),
    '$.provenance', json_patch(json_extract(document_json, '$.provenance'), json((
        SELECT json_group_object(
            substr(key, 1, length(key) - length('approvals')) || 'approval_reviewer',
            json_set(value,
                '$.source.layer', CASE json_extract(value, '$.source.layer')
                    WHEN 'builtin' THEN 'builtin' ELSE 'compiled' END,
                '$.source.file', NULL,
                '$.source.field', substr(json_extract(value, '$.source.field'), 1,
                    length(json_extract(value, '$.source.field')) - length('approvals')) || 'approval_reviewer'
            )
        )
        FROM json_each(document_json, '$.provenance') AS origin
        WHERE key LIKE '%.approvals'
          AND NOT EXISTS (
              SELECT 1 FROM json_each(document_json, '$.provenance') AS existing
              WHERE existing.key = substr(origin.key, 1, length(origin.key) - length('approvals')) || 'approval_reviewer'
          )
    )))
)
WHERE scope = 'run';

CREATE TRIGGER configuration_snapshots_are_append_only
BEFORE UPDATE ON configuration_snapshots
BEGIN
    SELECT RAISE(ABORT, 'configuration snapshots are append-only');
END;
