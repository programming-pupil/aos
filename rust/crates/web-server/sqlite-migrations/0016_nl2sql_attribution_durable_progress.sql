ALTER TABLE nl2sql_attribution_tasks
  ADD COLUMN progress_events_json TEXT NOT NULL DEFAULT '[]';
