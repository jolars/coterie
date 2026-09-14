-- Historical intents must reproduce their original commit on recovery.
UPDATE operations
SET result_json = json_set(result_json, '$.strategy', 'merge')
WHERE kind = 'workspace.integrate' AND result_json IS NOT NULL
  AND json_type(result_json, '$.strategy') IS NULL;
