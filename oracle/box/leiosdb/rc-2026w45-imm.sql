-- The schema sql_schema_imm of ouroboros-consensus 1f2984cc, the leios.imm.db of node release rc-2026w45.
CREATE TABLE IF NOT EXISTS ebs (
  ebSlot INTEGER NOT NULL,
  ebHashBytes BLOB NOT NULL,
  ebBytesSize INTEGER NOT NULL,
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
