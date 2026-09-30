-- no-transaction
-- Keep request lookup separate from user lookup: one concurrent build per file.
CREATE INDEX CONCURRENTLY idx_events_project_request_identity
    ON events (project_id, (data #>> '{tags,request.id}'), timestamp DESC, id DESC)
    WHERE jsonb_typeof(data #> '{tags,request.id}') = 'string'
      AND octet_length(data #>> '{tags,request.id}') BETWEEN 1 AND 200;
