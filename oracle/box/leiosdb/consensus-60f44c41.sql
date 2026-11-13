-- The schema sql_schema_imm of ouroboros-consensus 60f44c41, a LeiosDb layout none of node releases rc-2026w42 to rc-2026w45 writes.
CREATE TABLE IF NOT EXISTS ebs (
  ebSlot INTEGER NOT NULL,
  ebHashBytes BLOB NOT NULL,
  txWritten BLOB NOT NULL,
  txBounds BLOB NOT NULL,
  body BLOB NOT NULL,
  txs BLOB NOT NULL,
  PRIMARY KEY (ebSlot, ebHashBytes)
);
CREATE INDEX IF NOT EXISTS idx_ebs_ebHashBytes ON ebs(ebHashBytes);
