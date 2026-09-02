-- Indexes for the observations list surface (#527). The table is append-only
-- history that never shrinks, so a missing covering index means a full scan
-- and top-N sort of everything per call.

-- `list_observations` orders by (observed_at DESC, id DESC); with no filter,
-- or only `kind`, nothing covered the order.
CREATE INDEX idx_observations_at ON observations (observed_at DESC, id DESC);

-- kind-only listings (`observations list --kind`, and drift_service) get a
-- covering index for their filter-plus-order shape.
CREATE INDEX idx_observations_kind_at ON observations (kind, observed_at DESC);
