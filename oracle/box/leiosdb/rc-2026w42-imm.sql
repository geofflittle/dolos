-- The schema sql_schema_imm of ouroboros-consensus 840e789b, the leios.imm.db of node release rc-2026w42.
CREATE TABLE IF NOT EXISTS ebs (
  ebSlot INTEGER NOT NULL,
  ebHashBytes BLOB NOT NULL,
  ebBytesSize INTEGER NOT NULL,
  PRIMARY KEY (ebSlot, ebHashBytes)
);
CREATE INDEX IF NOT EXISTS idx_ebs_ebHashBytes ON ebs(ebHashBytes);
CREATE TABLE IF NOT EXISTS ebTxs (
  ebHashBytes BLOB NOT NULL,
  txOffset INTEGER NOT NULL,
  txHashBytes BLOB NOT NULL,
  txBytesSize INTEGER NOT NULL,
  PRIMARY KEY (ebHashBytes, txOffset)
);
CREATE TABLE IF NOT EXISTS ebTxBytes (
  ebHashBytes BLOB NOT NULL,
  txOffset INTEGER NOT NULL,
  filled INTEGER NOT NULL DEFAULT 0,
  txBytes BLOB NOT NULL,
  PRIMARY KEY (ebHashBytes, txOffset)
);
