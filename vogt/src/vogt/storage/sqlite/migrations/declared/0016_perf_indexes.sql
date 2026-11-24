-- Indexes for hot list surfaces that had no covering index (#527). Both
-- tables are append-only and never shrink, so the missing indexes degraded
-- linearly with all-time history.

-- `list_events(after, entity_id=...)` filters by entity_id and orders by seq,
-- but the only prior index was on `at` — so "history of WI-7" walked the PK
-- from seq 0 testing entity_id per row.
CREATE INDEX idx_events_entity ON events (entity_id, seq);

-- The default operator audit page orders by (revision DESC, at DESC, id DESC)
-- and no index covered it, so every page scanned and sorted the whole table.
-- `revision` is monotonic with insertion, so this matches the sort exactly.
CREATE INDEX idx_audit_order ON audit (revision DESC, at DESC, id DESC);
