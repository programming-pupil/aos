-- The ledger envelope remains a redacted, hash-verifiable projection. Runtime
-- recovery uses this encrypted payload only after integrity and ownership
-- checks, so JSONL is never needed as a second fact source.
ALTER TABLE agent_event_ledger ADD COLUMN raw_payload_ciphertext TEXT;
