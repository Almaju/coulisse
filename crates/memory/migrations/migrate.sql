-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

-- Add agent_name to the scores table so per-variant aggregation works for
-- A/B experiments. Existing rows pre-date experiments, so they get the
-- empty string — group-by queries for new variants ignore those.
ALTER TABLE scores ADD COLUMN agent_name TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_scores_agent ON scores(agent_name);
