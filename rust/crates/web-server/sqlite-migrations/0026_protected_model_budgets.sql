-- Stage-aware provider budget reservations. Protected parents are allocated
-- before ordinary model/tool work and child calls settle back into them.
ALTER TABLE resource_budget_entries ADD COLUMN purpose TEXT NOT NULL DEFAULT 'general';
ALTER TABLE resource_budget_entries ADD COLUMN parent_reservation_id TEXT;
ALTER TABLE resource_budget_entries ADD COLUMN committed_amount INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_resource_budget_parent
    ON resource_budget_entries(tenant_id, owner_scope, parent_reservation_id, state);
