-- The schema sql_schema_vol of ouroboros-consensus 1f2984cc, the leios.vol.db of node release rc-2026w45.
CREATE TABLE IF NOT EXISTS ebs (
  ebSlot INTEGER NOT NULL,
  ebHashBytes BLOB NOT NULL,
  ebBytesSize INTEGER NOT NULL,
  status INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (ebSlot, ebHashBytes)
);
CREATE INDEX IF NOT EXISTS idx_ebs_ebHashBytes ON ebs(ebHashBytes);
CREATE TABLE IF NOT EXISTS ebBodies (
  ebHashBytes BLOB NOT NULL PRIMARY KEY,
  txBounds BLOB NOT NULL,
  body BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS ebClosures (
  ebHashBytes BLOB NOT NULL PRIMARY KEY,
  txs BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ebs_sweepable ON ebs(ebSlot) WHERE status IN (0, 2);
CREATE INDEX IF NOT EXISTS idx_ebs_markedForGc ON ebs(ebHashBytes) WHERE status = 3;
CREATE INDEX IF NOT EXISTS idx_ebs_pinned ON ebs(ebSlot) WHERE status = 1;
