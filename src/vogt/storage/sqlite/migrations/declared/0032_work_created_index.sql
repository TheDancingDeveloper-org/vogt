-- WI-4638: work lists order by created_at, ref (WORK_SELECT ... ORDER BY
-- w.created_at, w.ref). Without this index every page sorts the whole table.
CREATE INDEX IF NOT EXISTS idx_work_items_created ON work_items (created_at, ref);
