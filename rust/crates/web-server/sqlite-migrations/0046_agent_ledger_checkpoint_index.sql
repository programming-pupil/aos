-- Keep session history recovery bounded by the newest runtime checkpoint.
-- The event type predicate is selective for long execution ledgers, while
-- sequence ordering lets SQLite satisfy MAX(sequence) from the same index.
CREATE INDEX IF NOT EXISTS idx_agent_event_ledger_checkpoint
    ON agent_event_ledger(tenant_id, thread_id, event_type, sequence);
