-- The backlog count and the ranked backlog read `latest_observations` newest
-- first, by (observed_at DESC, subject_key), and stop at 1000 rows (WI-4448).
-- Nothing covered that order, so every read sorted the whole current set. With
-- this index the read walks the newest rows and stops once it has 1000 open ones.

CREATE INDEX idx_latest_observed ON latest_observations (observed_at DESC, subject_key);
