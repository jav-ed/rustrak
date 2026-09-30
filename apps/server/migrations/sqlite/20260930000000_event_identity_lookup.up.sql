-- Exact selectors use string identifiers only. Partial byte-length guards keep
-- unrelated oversized SDK values ingestible and out of the lookup indexes.
-- SQLite holds its migration write transaction while these indexes are built.
CREATE INDEX idx_events_project_user_identity
    ON events (project_id, json_extract(data, '$.user.id'), timestamp DESC, id DESC)
    WHERE json_type(data, '$.user.id') = 'text'
      AND length(CAST(json_extract(data, '$.user.id') AS BLOB)) BETWEEN 1 AND 200;

CREATE INDEX idx_events_project_request_identity
    ON events (project_id, json_extract(data, '$.tags."request.id"'), timestamp DESC, id DESC)
    WHERE json_type(data, '$.tags."request.id"') = 'text'
      AND length(CAST(json_extract(data, '$.tags."request.id"') AS BLOB)) BETWEEN 1 AND 200;
